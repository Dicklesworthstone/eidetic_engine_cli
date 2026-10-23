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
    for (id, path) in [
        (WS, workspace.to_string_lossy().into_owned()),
        (OTHER, "/other-impact".to_owned()),
    ] {
        db.insert_workspace(id, &CreateWorkspaceInput { path, name: None })
            .unwrap();
    }
}

fn seed(db: &DbConnection, workspace: &str, number: u128, body: &str) -> String {
    let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
    db.insert_memory_with_timestamps(
        &id,
        &CreateMemoryInput {
            workspace_id: workspace.to_owned(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            content: body.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: Some("manual://impact-graph".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: None,
            valid_to: None,
        },
        BASE,
        BASE,
        &id,
    )
    .unwrap();
    id
}

fn surface() -> ImpactResolvedSurface {
    let anchor = CreateMemoryAnchorInput::from_raw(
        "query",
        MemoryAnchorKind::EnvVar,
        "EE_IMPACT_ROUTE",
        1.0,
        MemoryAnchorSource::Explicit,
        "test",
        0,
    )
    .unwrap();
    ImpactResolvedSurface {
        schema: super::super::IMPACT_SCHEMA_V1,
        kind: anchor.anchor_kind,
        anchor_value_hash: anchor.anchor_value_hash,
        redacted_anchor_value: anchor.redacted_anchor_value,
    }
}

fn read_graph(
    db: &DbConnection,
    excluded: &BTreeSet<String>,
    limit: usize,
) -> (Vec<ImpactResult>, ImpactGraphPhase) {
    let scope = MemoryScopeContext::for_workspace(
        std::path::Path::new("/impact-graph"),
        MemoryScope::Swarm,
        false,
    );
    let mut stats = scope.stats();
    let admission = MemoryAdmission::new(db, WS, &scope);
    read(&admission, &mut stats, &surface(), excluded, limit)
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
        db.execute_raw(&format!(
            "UPDATE memories SET {column} = '2021-01-01T00:00:00Z' WHERE id = '{root}'"
        ))
        .unwrap();
        assert!(
            read_graph(&db, &BTreeSet::new(), 8).0.is_empty(),
            "{column}"
        );
    }
}

#[test]
#[cfg(feature = "graph")]
fn held_target_cannot_be_reintroduced_through_a_different_surface() {
    let (db, root, target) = fixture();
    db.insert_feedback_quarantine(
        "fq_00000000000000000000000931",
        &CreateFeedbackQuarantineInput {
            workspace_id: WS.to_owned(),
            source_id: "PRIVATE-SOURCE".to_owned(),
            target_type: "memory".to_owned(),
            target_id: target,
            signal: "harmful".to_owned(),
            weight: 1.0,
            source_type: "outcome_observed".to_owned(),
            proposed_event_id: None,
            recorded_at: BASE.to_owned(),
            reason: "PRIVATE-REVIEW-REASON".to_owned(),
            event_reason: None,
            evidence_json: None,
            session_id: None,
            raw_event_hash: format!("blake3:{}", "a".repeat(64)),
        },
    )
    .unwrap();
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
    db.execute_raw(&format!(
        "UPDATE memory_anchors SET freshness_state = 'stale' WHERE memory_id = '{target}'"
    ))
    .unwrap();
    let stale = read_graph(&db, &excluded, 8).0;
    assert_eq!(stale.len(), 1);
    assert!(stale[0].score < current[0].score);
    assert_eq!(
        stale[0].graph_path.as_ref().unwrap()["anchorEvidence"][2]["freshnessState"],
        "stale"
    );
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
    assert_eq!(
        read_graph(&db, &BTreeSet::from([root]), 8).0[0].memory_id,
        target
    );
    let error = db
        .begin_read_snapshot()
        .expect_err("caller snapshot remains active");
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

fn hold_memory(db: &DbConnection, memory: &str) {
    db.insert_feedback_quarantine(
        "fq_00000000000000000000000933",
        &CreateFeedbackQuarantineInput {
            workspace_id: WS.to_owned(),
            source_id: "PRIVATE-IMPACT-REVIEW".to_owned(),
            target_type: "memory".to_owned(),
            target_id: memory.to_owned(),
            signal: "harmful".to_owned(),
            weight: 1.0,
            source_type: "outcome_observed".to_owned(),
            proposed_event_id: None,
            recorded_at: BASE.to_owned(),
            reason: "PRIVATE-IMPACT-REVIEW".to_owned(),
            event_reason: None,
            evidence_json: None,
            session_id: None,
            raw_event_hash: format!("blake3:{}", "a".repeat(64)),
        },
    )
    .unwrap();
}

#[test]
fn public_exact_impact_admits_live_sources_before_filling_the_limit() {
    const SECRET: &str = "sk-proj-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    for restriction in [
        "tombstoned",
        "superseded",
        "expired",
        "future",
        "sealed",
        "held",
        "placeholder",
        "foreign",
        "secret_body",
        "secret_provenance",
    ] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().canonicalize().unwrap();
        let path = workspace.join("impact.ee.db");
        let db = DbConnection::open_file(&path).unwrap();
        initialize(&db, &workspace);
        let hidden = seed(&db, WS, 1, "PRIVATE-IMPACT-CANARY for `EE_IMPACT_ROUTE`.");
        let live = seed(&db, WS, 2, ROOT_BODY);
        match restriction {
            "tombstoned" | "superseded" | "expired" | "future" => {
                let (column, value) = match restriction {
                    "tombstoned" => ("tombstoned_at", BASE),
                    "superseded" => ("superseded_at", BASE),
                    "expired" => ("valid_to", BASE),
                    _ => ("valid_from", "2099-01-01T00:00:00Z"),
                };
                db.execute_raw(&format!(
                    "UPDATE memories SET {column} = '{value}' WHERE id = '{hidden}'"
                ))
                .unwrap();
            }
            "sealed" => db
                .insert_memory_seal(&hidden, &format!("blake3:{}", "a".repeat(64)), BASE)
                .unwrap(),
            "held" => hold_memory(&db, &hidden),
            "placeholder" => db
                .execute_raw(&format!(
                    "UPDATE memories SET content = '{}' WHERE id = '{hidden}'",
                    crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT,
                ))
                .unwrap(),
            "foreign" => db
                .execute_raw(&format!(
                    "UPDATE memories SET workspace_id = '{OTHER}' WHERE id = '{hidden}'"
                ))
                .unwrap(),
            "secret_body" => db
                .execute_raw(&format!(
                    "UPDATE memories SET content = 'PRIVATE-IMPACT-CANARY {SECRET}' WHERE id = '{hidden}'"
                ))
                .unwrap(),
            "secret_provenance" => db
                .execute_raw(&format!(
                    "UPDATE memories SET provenance_uri = 'https://example.invalid/{SECRET}' WHERE id = '{hidden}'"
                ))
                .unwrap(),
            _ => unreachable!(),
        }
        // Keep the stale anchor in place: the result limit must be filled by
        // admitted memory, rather than trusting an exact index/anchor match.
        assert!(!db.list_memory_anchors(&hidden).unwrap().is_empty());
        let before = std::fs::read(&path).unwrap();
        let report = super::super::run_impact(&super::super::ImpactOptions {
            workspace_path: workspace.clone(),
            database_path: Some(path.clone()),
            index_dir: Some(workspace.join("absent-index")),
            surface: super::super::ImpactSurfaceQuery {
                kind: MemoryAnchorKind::EnvVar,
                value: "EE_IMPACT_ROUTE".to_owned(),
            },
            limit: 1,
            speed: crate::search::SpeedMode::Instant,
            source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
            strict_source_mode: false,
            memory_scope: MemoryScope::Swarm,
            strict_scope: false,
        })
        .unwrap();
        assert_eq!(report.results.len(), 1, "{restriction}");
        assert_eq!(report.results[0].memory_id, live, "{restriction}");
        assert_eq!(report.exact_anchor_count, 1, "{restriction}");
        assert_eq!(
            report.fallback_status,
            super::super::ImpactFallbackStatus::SkippedLimitFilled,
            "{restriction}"
        );
        let rendered = report.data_json().to_string();
        assert!(!rendered.contains("PRIVATE-IMPACT"), "{restriction}");
        assert!(!rendered.contains(SECRET), "{restriction}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "{restriction}");
    }
}

#[test]
fn fallback_rehydration_cannot_reintroduce_sealed_or_secret_memory() {
    use crate::core::profile::{OperatingProfile, RuntimeProfileReport};
    use crate::core::search::{
        ScoreSource, SearchHit, SearchReport, SearchSourceMode, SearchStatus,
    };
    use crate::models::model_registry::EmbedBackend;

    let (db, hidden, live) = fixture();
    db.insert_memory_seal(&hidden, &format!("blake3:{}", "a".repeat(64)), BASE)
        .unwrap();
    let secret = seed(&db, WS, 3, TARGET_BODY);
    db.execute_raw(&format!(
        "UPDATE memories SET provenance_uri = 'https://example.invalid/sk-proj-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789' WHERE id = '{secret}'"
    ))
    .unwrap();
    // A cached/index result is only a candidate. Recheck the authoritative
    // source even when search previously admitted or redacted its own hit.
    let report = SearchReport {
        status: SearchStatus::Success,
        embed_backend: EmbedBackend::HashFallback,
        query: "impact route".to_owned(),
        requested_limit: 3,
        results: [&hidden, &secret, &live]
            .into_iter()
            .map(|id| SearchHit {
                doc_id: id.clone(),
                score: 0.91,
                source: ScoreSource::Lexical,
                fast_score: None,
                quality_score: None,
                lexical_score: Some(0.91),
                rerank_score: None,
                metadata: None,
                explanation: None,
            })
            .collect(),
        elapsed_ms: 0.0,
        errors: Vec::new(),
        degraded: Vec::new(),
        runtime_profile: RuntimeProfileReport::for_profile(
            OperatingProfile::Workstation,
            "impact_admission_test",
        ),
        rerank_configured_mode: crate::config::SearchRerankMode::Auto,
        rerank_configured_top_k: 50,
        rerank_runtime_available: false,
        relevance_floor_applied: None,
        candidates_below_floor: 0,
        query_assist: None,
        source_mode_requested: SearchSourceMode::LexicalOnly,
        source_mode_applied: SearchSourceMode::LexicalOnly,
        source_mode_fallback: false,
        strict_source_mode: false,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        index_freshness: None,
    };
    let scope = MemoryScopeContext::for_workspace(
        std::path::Path::new("/impact-graph"),
        MemoryScope::Swarm,
        false,
    );
    let snapshot = Snapshot::begin(&db).unwrap();
    let admission = MemoryAdmission::new(&db, WS, &scope);
    let mut seen = BTreeSet::new();
    let mut results = Vec::new();
    let mut count = 0;
    super::super::append_fallback_results(
        &admission,
        &report,
        &mut seen,
        1,
        &mut results,
        &mut count,
    )
    .unwrap();
    snapshot.finish().unwrap();
    assert_eq!(count, 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].memory_id, live);
    assert_eq!(seen, BTreeSet::from([live]));
    let rendered = results[0].data_json().to_string();
    assert!(!rendered.contains("sk-proj-"));
    assert!(!rendered.contains(&hidden));
    db.begin_read_snapshot().unwrap();
    db.rollback_read_snapshot().unwrap();
}

#[test]
fn impact_does_not_create_a_missing_source_database() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("missing-impact.ee.db");
    let result = super::super::run_impact(&super::super::ImpactOptions {
        workspace_path: root.path().to_path_buf(),
        database_path: Some(path.clone()),
        index_dir: None,
        surface: super::super::ImpactSurfaceQuery {
            kind: MemoryAnchorKind::EnvVar,
            value: "EE_IMPACT_ROUTE".to_owned(),
        },
        limit: 1,
        speed: crate::search::SpeedMode::Instant,
        source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
        strict_source_mode: false,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
    });
    assert!(result.is_err());
    assert!(!path.exists());
}
