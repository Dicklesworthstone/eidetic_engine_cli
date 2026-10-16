//! Connected projection, admission and public-impact regressions.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::db::{CreateFeedbackQuarantineInput, CreateMemoryInput, CreateWorkspaceInput};
use crate::models::{CreateMemoryAnchorInput, MemoryScope};

const WS: &str = "wsp_00000000000000000000000931";
const OTHER: &str = "wsp_00000000000000000000000932";
const ROOT_BODY: &str = "Cache configuration for `EE_IMPACT_ROUTE` and `src/secondary.rs`.";
const TARGET_BODY: &str = "Cache recovery for `src/secondary.rs`.";
const BASE: &str = "2020-01-01T00:00:00Z";

fn initialize(db: &DbConnection, workspace: &std::path::Path) {
    db.migrate().unwrap();
    for (id, path) in [(WS, workspace.to_string_lossy().into_owned()), (OTHER, "/other-impact".to_owned())] {
        db.insert_workspace(id, &CreateWorkspaceInput { path, name: None }).unwrap();
    }
}

fn seed(db: &DbConnection, workspace: &str, number: u128, body: &str) -> String {
    let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
    db.insert_memory_with_timestamps(&id, &CreateMemoryInput {
        workspace_id: workspace.to_owned(), level: "semantic".to_owned(), kind: "fact".to_owned(),
        content: body.to_owned(), workflow_id: None, confidence: 0.9, utility: 0.5, importance: 0.5,
        provenance_uri: Some("manual://impact-graph".to_owned()), trust_class: "human_explicit".to_owned(),
        trust_subclass: None, tags: Vec::new(), valid_from: None, valid_to: None,
    }, BASE, BASE, &id).unwrap();
    id
}

fn surface() -> ImpactResolvedSurface {
    let anchor = CreateMemoryAnchorInput::from_raw(
        "query", MemoryAnchorKind::EnvVar, "EE_IMPACT_ROUTE", 1.0, MemoryAnchorSource::Explicit, "test", 0,
    ).unwrap();
    ImpactResolvedSurface { schema: super::super::IMPACT_SCHEMA_V1, kind: anchor.anchor_kind,
        anchor_value_hash: anchor.anchor_value_hash, redacted_anchor_value: anchor.redacted_anchor_value }
}

fn read_graph(db: &DbConnection, excluded: &BTreeSet<String>, limit: usize) -> (Vec<ImpactResult>, ImpactGraphPhase) {
    let scope = MemoryScopeContext::for_workspace(std::path::Path::new("/impact-graph"), MemoryScope::Swarm, false);
    let mut stats = scope.stats();
    read(db, WS, &scope, &mut stats, &surface(), excluded, limit)
}

fn fixture() -> (DbConnection, String, String) {
    let db = DbConnection::open_memory().unwrap();
    initialize(&db, std::path::Path::new("/impact-graph"));
    let root = seed(&db, WS, 1, ROOT_BODY);
    let target = seed(&db, WS, 2, TARGET_BODY);
    (db, root, target)
}

#[test]
#[cfg(feature = "graph")]
fn graph_connects_a_co_mentioned_surface_without_a_stored_memory_link() {
    let (db, root, target) = fixture();
    let before = db.list_memory_anchors(&root).unwrap();
    assert!(db.list_all_memory_links(None).unwrap().is_empty());
    let (results, phase) = read_graph(&db, &BTreeSet::from([root.clone()]), 8);
    assert_eq!(phase.status, "ok");
    assert!(!phase.truncated);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].memory_id, target);
    assert_eq!(results[0].match_type, ImpactMatchType::GraphNeighbor);
    let path = results[0].graph_path.as_ref().unwrap();
    assert_eq!(path["viaMemoryId"], root);
    assert_eq!(path["anchorEvidence"].as_array().unwrap().len(), 3);
    assert_eq!(path["path"].as_array().unwrap().len(), 3);
    assert!(!path.to_string().contains("src/secondary.rs"));
    assert!(!path.to_string().contains("EE_IMPACT_ROUTE"));
    assert_eq!(db.list_memory_anchors(&root).unwrap(), before);
    assert!(db.list_all_memory_links(None).unwrap().is_empty());
    assert_eq!(read_graph(&db, &BTreeSet::from([root]), 8).0, results);
}

#[test]
#[cfg(feature = "graph")]
fn same_surface_in_another_workspace_cannot_extend_the_path() {
    let (db, root, target) = fixture();
    seed(&db, OTHER, 3, TARGET_BODY);
    let (results, _) = read_graph(&db, &BTreeSet::from([root, target]), 8);
    assert!(results.is_empty());
}

#[test]
#[cfg(feature = "graph")]
fn tombstoned_or_expired_bridges_cannot_relay_otherwise_live_advice() {
    for column in ["tombstoned_at", "valid_to", "superseded_at"] {
        let (db, root, _) = fixture();
        db.execute_raw(&format!("UPDATE memories SET {column} = '2021-01-01T00:00:00Z' WHERE id = '{root}'")).unwrap();
        assert!(read_graph(&db, &BTreeSet::new(), 8).0.is_empty(), "{column}");
    }
}

#[test]
#[cfg(feature = "graph")]
fn held_target_cannot_be_reintroduced_through_a_different_surface() {
    let (db, root, target) = fixture();
    db.insert_feedback_quarantine("fq_00000000000000000000000931", &CreateFeedbackQuarantineInput {
        workspace_id: WS.to_owned(), source_id: "PRIVATE-SOURCE".to_owned(), target_type: "memory".to_owned(),
        target_id: target, signal: "harmful".to_owned(), weight: 1.0, source_type: "outcome_observed".to_owned(),
        proposed_event_id: None, recorded_at: BASE.to_owned(), reason: "PRIVATE-REVIEW-REASON".to_owned(),
        event_reason: None, evidence_json: None, session_id: None, raw_event_hash: format!("blake3:{}", "a".repeat(64)),
    }).unwrap();
    let (results, phase) = read_graph(&db, &BTreeSet::from([root]), 8);
    assert!(results.is_empty());
    assert!(!phase.data_json().to_string().contains("PRIVATE"));
}

#[test]
#[cfg(feature = "graph")]
fn confidence_then_identity_ranking_is_deduplicated_and_respects_prior_tiers() {
    let (db, root, target) = fixture();
    let second_root = seed(&db, WS, 4, ROOT_BODY);
    let second_target = seed(&db, WS, 3, TARGET_BODY);
    let excluded = BTreeSet::from([root, second_root]);
    let all = read_graph(&db, &excluded, 20).0;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].memory_id, target);
    assert_eq!(all[1].memory_id, second_target);
    assert_eq!(read_graph(&db, &excluded, 1).0, all[..1]);
    let mut excluded = excluded;
    excluded.insert(target);
    assert_eq!(read_graph(&db, &excluded, 1).0[0].memory_id, second_target);
}

#[test]
#[cfg(feature = "graph")]
fn stale_anchor_support_is_downranked_not_silently_removed() {
    let (db, root, target) = fixture();
    let excluded = BTreeSet::from([root]);
    let current = read_graph(&db, &excluded, 8).0;
    db.execute_raw(&format!("UPDATE memory_anchors SET freshness_state = 'stale' WHERE memory_id = '{target}'")).unwrap();
    let stale = read_graph(&db, &excluded, 8).0;
    assert_eq!(stale.len(), 1);
    assert!(stale[0].score < current[0].score);
    assert_eq!(stale[0].graph_path.as_ref().unwrap()["anchorEvidence"][2]["freshnessState"], "stale");
}

#[test]
#[cfg(feature = "graph")]
fn mixed_anchor_generations_are_not_evidence_of_co_mention() {
    let (db, root, _) = fixture();
    db.execute_raw(&format!("UPDATE memory_anchors SET generation = generation + 1 WHERE memory_id = '{root}' AND anchor_kind = 'path'")).unwrap();
    assert!(read_graph(&db, &BTreeSet::new(), 8).0.is_empty());
}

#[test]
fn zero_limit_never_needs_a_migrated_graph_store() {
    let db = DbConnection::open_memory().unwrap();
    let (results, phase) = read_graph(&db, &BTreeSet::new(), 0);
    assert!(results.is_empty());
    assert_eq!(phase.status, "skipped_limit_filled");
    assert_eq!(phase.source_rows, 0);
    assert_eq!(phase.paths_examined, 0);
}

#[test]
#[cfg(feature = "graph")]
fn unavailable_graph_does_not_expose_database_errors_or_keep_its_snapshot() {
    let db = DbConnection::open_memory().unwrap();
    let (results, phase) = read_graph(&db, &BTreeSet::new(), 8);
    assert!(results.is_empty());
    assert_eq!(phase.reason, Some("graph_source_unavailable"));
    assert!(!phase.data_json().to_string().contains("memory_anchors"));
    db.begin_read_snapshot().unwrap();
    db.rollback_read_snapshot().unwrap();
}

#[test]
#[cfg(feature = "graph")]
fn caller_transaction_is_borrowed_and_remains_owned_by_the_caller() {
    let (db, root, target) = fixture();
    db.begin_read_snapshot().unwrap();
    assert_eq!(read_graph(&db, &BTreeSet::from([root]), 8).0[0].memory_id, target);
    let error = db.begin_read_snapshot().expect_err("caller snapshot remains active");
    assert!(crate::db::db_error_is_nested_transaction(&error));
    db.rollback_read_snapshot().unwrap();
}

#[test]
#[cfg(not(feature = "graph"))]
fn disabled_graph_feature_does_not_touch_source_tables() {
    let db = DbConnection::open_memory().unwrap();
    let (results, phase) = read_graph(&db, &BTreeSet::new(), 8);
    assert!(results.is_empty());
    assert_eq!(phase.reason, Some("graph_feature_disabled"));
}
