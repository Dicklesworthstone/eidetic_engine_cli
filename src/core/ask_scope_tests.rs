#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::ask::{AskRequest, ask_data_json, evaluate_ask};
use crate::db::{CreateMemoryInput, CreateWorkspaceInput, InsertTeamMemberInput};
use crate::models::WorkspaceId;

fn fixture() -> (tempfile::TempDir, DbConnection, String) {
    let root = tempfile::tempdir().unwrap();
    let db = DbConnection::open_file(&root.path().join("ask.db")).unwrap();
    db.migrate().unwrap();
    let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_bytes([44; 16])).to_string();
    db.insert_workspace(&workspace, &CreateWorkspaceInput {
        path: root.path().to_string_lossy().into_owned(), name: None,
    }).unwrap();
    (root, db, workspace)
}

fn seed(db: &DbConnection, workspace: &str, number: usize, actor: &str, trust: &str, tags: &[&str]) -> String {
    let id = format!("mem_{number:026}");
    db.insert_memory(&id, &CreateMemoryInput {
        workspace_id: workspace.to_owned(), content: format!("Run cargo fmt before release. Source {number}."),
        level: "procedural".to_owned(), kind: "rule".to_owned(), workflow_id: None,
        confidence: 0.9, utility: 0.5, importance: 0.5,
        provenance_uri: Some(format!("manual://scope/{number}")),
        trust_class: trust.to_owned(), trust_subclass: Some(format!("agent:{actor}")),
        tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        valid_from: Some("2020-01-01T00:00:00Z".to_owned()), valid_to: None,
    }).unwrap();
    id
}

fn load(db: &DbConnection, workspace: &str, scope: MemoryScope, actor: Option<&str>) -> AskCorpus {
    load_corpus_with_scope_boundary(db, workspace, Utc::now(), || {
        let mut context = scope_context(db, workspace, scope)?;
        context.current_agent = actor.map(str::to_owned);
        Ok(context)
    }, || Ok(())).unwrap()
}

fn ids(corpus: &AskCorpus) -> Vec<&str> {
    corpus.candidates.iter().map(|candidate| candidate.memory_id.as_str()).collect()
}

fn member(db: &DbConnection, workspace: &str, id: &str, name: &str, state: &str) {
    db.insert_team_member(&InsertTeamMemberInput {
        member_id: id.to_owned(), team_id: "team_ask".to_owned(), workspace_id: workspace.to_owned(),
        display_name: name.to_owned(), state: state.to_owned(), is_self: false,
        origin_node_id: format!("node_{name}"), bound_via: "invite_ceremony".to_owned(),
        joined_at: "2020-01-01T00:00:00Z".to_owned(),
    }).unwrap();
}

#[test]
fn self_scope_requires_identity_and_excludes_other_agents_from_hints() {
    let (_root, db, workspace) = fixture();
    let own = seed(&db, &workspace, 1, "Alice", "human_explicit", &[]);
    let other = seed(&db, &workspace, 2, "Bob", "human_explicit", &[]);
    let corpus = load(&db, &workspace, MemoryScope::SelfOnly, Some("Alice"));
    assert_eq!(ids(&corpus), [own.as_str()]);
    let report = evaluate_ask(&AskRequest {
        question: "Run cargo fmt before release".to_owned(), min_confidence: 1.0,
        ..AskRequest::default()
    }, &corpus.candidates);
    assert!(report.abstained);
    assert!(!ask_data_json(&report).to_string().contains(&other));
    assert!(load(&db, &workspace, MemoryScope::SelfOnly, None).candidates.is_empty());
}

#[test]
fn verified_scope_uses_native_trust_classes_not_asserted_agent_names() {
    let (_root, db, workspace) = fixture();
    let human = seed(&db, &workspace, 1, "Alice", "human_explicit", &[]);
    let validated = seed(&db, &workspace, 2, "Bob", "agent_validated", &[]);
    seed(&db, &workspace, 3, "Alice", "agent_assertion", &[]);
    seed(&db, &workspace, 4, "Alice", "cass_evidence", &[]);
    assert_eq!(ids(&load(&db, &workspace, MemoryScope::Verified, None)), [human.as_str(), validated.as_str()]);
}

#[test]
fn global_scope_accepts_only_explicit_tags_without_workspace_expansion() {
    let (_root, db, workspace) = fixture();
    let global = seed(&db, &workspace, 1, "Alice", "human_explicit", &["global"]);
    let rule = seed(&db, &workspace, 2, "Bob", "human_explicit", &["house_rule"]);
    seed(&db, &workspace, 3, "Alice", "human_explicit", &["global-ish"]);
    let foreign = WorkspaceId::from_uuid(uuid::Uuid::from_bytes([45; 16])).to_string();
    db.insert_workspace(&foreign, &CreateWorkspaceInput { path: "/ask-other-workspace".to_owned(), name: None }).unwrap();
    seed(&db, &foreign, 4, "Alice", "human_explicit", &["global"]);
    assert_eq!(ids(&load(&db, &workspace, MemoryScope::Global, None)), [global.as_str(), rule.as_str()]);
    assert_eq!(load(&db, &workspace, MemoryScope::Workspace, None).candidates.len(), 3);
    assert_eq!(load(&db, &workspace, MemoryScope::Swarm, None).candidates.len(), 3);
}

#[test]
fn team_scope_uses_active_workspace_roster_and_native_node_attribution() {
    let (_root, db, workspace) = fixture();
    let own = seed(&db, &workspace, 1, "Alice", "human_explicit", &[]);
    let active = seed(&db, &workspace, 2, "Bob", "human_explicit", &[]);
    let node = seed(&db, &workspace, 3, "node_Bob", "human_explicit", &[]);
    seed(&db, &workspace, 4, "Carol", "human_explicit", &[]);
    seed(&db, &workspace, 5, "Mallory", "human_explicit", &[]);
    member(&db, &workspace, "mbr_00000000000000000000000000000001", "Bob", "active");
    member(&db, &workspace, "mbr_00000000000000000000000000000002", "Carol", "removed");
    let foreign = WorkspaceId::from_uuid(uuid::Uuid::from_bytes([46; 16])).to_string();
    db.insert_workspace(&foreign, &CreateWorkspaceInput { path: "/ask-foreign-roster".to_owned(), name: None }).unwrap();
    member(&db, &foreign, "mbr_00000000000000000000000000000003", "Mallory", "active");
    assert_eq!(ids(&load(&db, &workspace, MemoryScope::Team, Some("Alice"))), [own.as_str(), active.as_str(), node.as_str()]);
}

#[test]
fn tag_changes_during_read_are_visible_only_to_the_next_snapshot() {
    let (root, db, workspace) = fixture();
    let id = seed(&db, &workspace, 1, "Alice", "human_explicit", &["global"]);
    let writer = DbConnection::open_file(&root.path().join("ask.db")).unwrap();
    let corpus = load_corpus_with_scope_boundary(&db, &workspace, Utc::now(), || {
        scope_context(&db, &workspace, MemoryScope::Global)
    }, || {
        writer.with_transaction(|| writer.remove_memory_tags(&id, &["global".to_owned()])).unwrap();
        Ok(())
    }).unwrap();
    assert_eq!(ids(&corpus), [id.as_str()]);
    assert!(load(&db, &workspace, MemoryScope::Global, None).candidates.is_empty());
}

#[test]
fn team_revocation_during_read_does_not_mix_roster_generations() {
    let (root, db, workspace) = fixture();
    let id = seed(&db, &workspace, 1, "Bob", "human_explicit", &[]);
    member(&db, &workspace, "mbr_00000000000000000000000000000001", "Bob", "active");
    let writer = DbConnection::open_file(&root.path().join("ask.db")).unwrap();
    let corpus = load_corpus_with_scope_boundary(&db, &workspace, Utc::now(), || {
        let mut context = scope_context(&db, &workspace, MemoryScope::Team)?;
        context.current_agent = None;
        Ok(context)
    }, || {
        writer.with_transaction(|| {
            writer.execute_raw("UPDATE team_members SET state = 'removed' WHERE member_id = 'mbr_00000000000000000000000000000001'")?;
            Ok(())
        }).unwrap();
        Ok(())
    }).unwrap();
    assert_eq!(ids(&corpus), [id.as_str()]);
    assert!(load(&db, &workspace, MemoryScope::Team, None).candidates.is_empty());
}

#[test]
fn unavailable_scope_metadata_fails_closed_and_releases_the_snapshot() {
    let (_root, db, workspace) = fixture();
    seed(&db, &workspace, 1, "Alice", "human_explicit", &["global"]);
    db.execute_raw("ALTER TABLE memory_tags RENAME TO unavailable_tags").unwrap();
    let error = load_scoped_ask_corpus(&db, &workspace, Utc::now(), MemoryScope::Global).unwrap_err();
    assert!(!error.message().contains("unavailable_tags"));
    db.begin_read_snapshot().unwrap();
    db.commit_read_snapshot().unwrap();
    assert_eq!(load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap().candidates.len(), 1);
}

#[test]
fn global_tag_batches_do_not_truncate_the_candidate_corpus() {
    let (_root, db, workspace) = fixture();
    db.with_transaction(|| {
        for number in 1..=260 {
            seed(&db, &workspace, number, "Alice", "human_explicit", &["global"]);
        }
        Ok(())
    }).unwrap();
    let corpus = load(&db, &workspace, MemoryScope::Global, None);
    assert_eq!(corpus.candidates.len(), 260);
    assert_eq!(corpus.candidates.last().unwrap().memory_id, format!("mem_{:026}", 260));
}
