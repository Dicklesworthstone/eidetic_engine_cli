#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::db::{
    CreateFeedbackEventInput, CreateMemoryInput, CreateProceduralRuleInput, CreateWorkspaceInput,
};

const BODY: &str = "qualityquasar release guidance preserves the authoritative source.";

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    db: DbConnection,
    workspace: String,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir(root.join(".ee")).unwrap();
        std::fs::write(
            root.join(".ee/config.toml"),
            "[memory]\ninclude_global = false\n",
        )
        .unwrap();
        let db = DbConnection::open_file(root.join(".ee/ee.db")).unwrap();
        db.migrate().unwrap();
        let workspace = crate::core::workspace::stable_workspace_id(&root);
        db.insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: root.display().to_string(),
                name: None,
            },
        )
        .unwrap();
        Self { _directory: directory, root, db, workspace }
    }

    fn options(&self) -> SearchOptions {
        SearchOptions {
            workspace_path: self.root.clone(),
            database_path: Some(self.root.join(".ee/ee.db")),
            index_dir: None,
            query: "qualityquasar".to_owned(),
            limit: 16,
            speed: SpeedMode::Instant,
            explain: true,
            as_of: None,
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::LexicalOnly,
            strict_source_mode: false,
            memory_scope: MemoryScope::Workspace,
            strict_scope: false,
        }
    }

    fn memory(
        &self,
        number: u128,
        confidence: f32,
        utility: f32,
        created: DateTime<Utc>,
    ) -> String {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db.insert_memory(
            &id,
            &CreateMemoryInput {
                workspace_id: self.workspace.clone(),
                content: BODY.to_owned(),
                level: "semantic".to_owned(),
                kind: "note".to_owned(),
                workflow_id: None,
                confidence,
                utility,
                importance: 0.5,
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                provenance_uri: Some("manual://quality-scoring".to_owned()),
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        ).unwrap();
        let created = created.to_rfc3339();
        self.db.execute_raw(&format!(
            "UPDATE memories SET created_at = '{created}', updated_at = '{created}' WHERE id = '{id}'"
        )).unwrap();
        id
    }

    fn harmful(&self, id: &str, number: u128) {
        self.db.insert_feedback_event(
            &format!("fb_{number:026}"),
            &CreateFeedbackEventInput {
                workspace_id: self.workspace.clone(),
                target_type: "memory".to_owned(),
                target_id: id.to_owned(),
                signal: "harmful".to_owned(),
                weight: 1.0,
                source_type: "outcome_observed".to_owned(),
                source_id: None,
                reason: None,
                evidence_json: None,
                session_id: None,
            },
        ).unwrap();
    }

    fn search(&self) -> SearchReport {
        let reader = DbConnection::open_file_read_only(self.root.join(".ee/ee.db")).unwrap();
        run_search_with_read_connection(&self.options(), &reader).unwrap()
    }
}

#[test]
fn real_search_orders_equal_lexical_memories_by_each_authoritative_quality_signal() {
    for changed in ["recency", "confidence", "utility", "harmfulPenalty"] {
        let fixture = Fixture::new();
        let now = Utc::now() - chrono::Duration::seconds(1);
        let weak = fixture.memory(
            1,
            if changed == "confidence" { 0.1 } else { 1.0 },
            if changed == "utility" { 0.0 } else { 1.0 },
            if changed == "recency" { now - chrono::Duration::days(30) } else { now },
        );
        let strong = fixture.memory(2, 1.0, 1.0, now);
        if changed == "harmfulPenalty" {
            fixture.harmful(&weak, 1);
        }
        let report = fixture.search();
        assert_eq!(report.status, SearchStatus::Success);
        assert_eq!(report.results.len(), 2);
        assert_eq!(report.results[0].doc_id, strong, "{changed}");
        assert_eq!(report.results[1].doc_id, weak);
        assert_eq!(report.results[0].lexical_score, report.results[1].lexical_score);
        assert_eq!(report.results[0].relevance_score(), report.results[1].relevance_score());
        assert!(report.results[0].ranking_score() > report.results[1].ranking_score());
        let components = report.results[1].ranking_components_json().unwrap();
        assert_eq!(components["observedSignals"][changed], true);
        assert_eq!(components["observedSignals"]["graphCentrality"], false);
        assert!(components["inputs"]["graphCentrality"].is_null());
        assert_eq!(components["components"]["graphCentrality"], 1.0);
        assert!(report.results[1].explanation.as_ref().unwrap().factors.iter()
            .any(|factor| factor.name == changed && factor.value < 1.0));
        let wire = report.data_json();
        assert_eq!(
            wire["results"][0]["metadata"]["qualityScoring"]["schema"],
            SEARCH_SCORING_POLICY_V1,
        );
        assert!(wire["results"][0]["metadata"].get(SEARCH_QUALITY_SCORING_KEY).is_none());
    }
}

#[test]
fn native_rule_maturity_and_negative_counters_affect_real_search() {
    let fixture = Fixture::new();
    for (number, maturity, negative) in [
        (1_u128, "candidate", 0),
        (2, "validated", 1),
        (3, "validated", 0),
    ] {
        let id = crate::models::RuleId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        fixture.db.insert_procedural_rule(
            &id,
            &CreateProceduralRuleInput {
                workspace_id: fixture.workspace.clone(),
                content: BODY.to_owned(),
                confidence: 1.0,
                utility: 1.0,
                importance: 0.5,
                trust_class: "human_explicit".to_owned(),
                scope: "workspace".to_owned(),
                scope_pattern: None,
                maturity: maturity.to_owned(),
                protected: false,
                source_memory_ids: Vec::new(),
                tags: Vec::new(),
            },
        ).unwrap();
        fixture.db.execute_raw(&format!(
            "UPDATE procedural_rules SET created_at = '2026-01-01T00:00:00Z', negative_feedback_count = {negative} WHERE id = '{id}'"
        )).unwrap();
    }
    let report = fixture.search();
    let expected: Vec<String> = [3_u128, 2, 1].into_iter()
        .map(|number| crate::models::RuleId::from_uuid(uuid::Uuid::from_u128(number)).to_string())
        .collect();
    assert_eq!(
        report.results.iter().map(|hit| hit.doc_id.clone()).collect::<Vec<_>>(),
        expected,
    );
    assert_eq!(
        report.results[2].ranking_components_json().unwrap()["components"]["maturity"], 0.5,
    );
    assert_eq!(
        report.results[1].ranking_components_json().unwrap()["inputs"]["harmfulCount"], 1,
    );
}

#[test]
fn published_search_and_diagnostics_overretrieve_before_quality_top_one() {
    let fixture = Fixture::new();
    let now = Utc::now() - chrono::Duration::seconds(1);
    fixture.memory(1, 0.1, 1.0, now);
    let strong = fixture.memory(2, 1.0, 1.0, now);
    let mut options = fixture.options();
    options.limit = 1;
    let _embedder = crate::core::index::install_test_hash_workspace_embedder(&fixture.workspace);
    fixture.db.close().unwrap();
    let rebuild = crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: options.database_path.clone(),
        index_dir: None,
        dry_run: false,
    }).unwrap();
    assert_eq!(rebuild.status, crate::core::index::IndexRebuildStatus::Success);
    let reader = DbConnection::open_file_read_only(options.resolve_database_path()).unwrap();
    let report = run_search_with_read_connection(&options, &reader).unwrap();
    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].doc_id, strong);
    let diagnostic = run_diag_search(&options).unwrap();
    assert_eq!(diagnostic.final_report.results.len(), 1);
    assert_eq!(diagnostic.final_report.results[0].doc_id, strong);
}

#[test]
fn unavailable_feedback_is_neutral_and_never_claims_observed_zero_harm() {
    let fixture = Fixture::new();
    let id = fixture.memory(1, 0.8, 0.7, Utc::now());
    let mut hits = fixture.search().results;
    fixture.db.execute_raw(
        "ALTER TABLE feedback_events RENAME TO unavailable_feedback_events",
    ).unwrap();
    let mut degraded = Vec::new();
    apply_quality_scoring(
        &fixture.options(), &mut hits, SearchScoringConfig::default(), Utc::now(),
        Some(&fixture.db), None, None, &mut degraded,
    );
    let scoring = hits.iter().find(|hit| hit.doc_id == id)
        .unwrap().ranking_components_json().unwrap();
    assert!(scoring["inputs"]["harmfulCount"].is_null());
    assert_eq!(scoring["observedSignals"]["harmfulPenalty"], false);
    assert_eq!(scoring["components"]["harmfulPenalty"], 1.0);
    assert!(degraded.iter().any(|entry| entry.code == "search_quality_signals_unavailable"));
}

#[test]
fn quality_reorders_reranked_hits_and_preserves_bonus_distinctions_in_pack_projection() {
    let fixture = Fixture::new();
    let now = Utc::now();
    let weak = fixture.memory(1, 0.9, 1.0, now);
    let strong = fixture.memory(2, 1.0, 1.0, now);
    let mut hits = fixture.search().results;
    for hit in &mut hits {
        hit.source = ScoreSource::Reranked;
        hit.score = if hit.doc_id == weak { 0.99 } else { 0.98 };
        hit.rerank_score = Some(hit.score);
    }
    apply_quality_scoring(
        &fixture.options(), &mut hits, SearchScoringConfig::default(), now,
        Some(&fixture.db), None, None, &mut Vec::new(),
    );
    assert_eq!(hits[0].doc_id, strong);
    assert!(hits[0].ranking_score() > 1.0 && hits[1].ranking_score() > 1.0);
    assert!(hits[0].ranking_relevance_score() > hits[1].ranking_relevance_score());
    assert!(hits.iter().all(|hit| (0.0..=1.0).contains(&hit.ranking_relevance_score())));
}

#[test]
fn handoff_rejects_new_or_mutated_feedback_and_changed_reference_time() {
    let fixture = Fixture::new();
    let id = fixture.memory(1, 1.0, 1.0, Utc::now());
    let options = fixture.options();
    let handoff = run_pack_search(&options).unwrap();
    assert!(handoff.snapshot_matches(&options, &fixture.db));
    let mut historical = options.clone();
    historical.as_of = Some(Utc::now());
    assert!(!handoff.matches_request(&historical));
    fixture.harmful(&id, 1);
    assert!(!handoff.snapshot_matches(&options, &fixture.db));
    let with_feedback = run_pack_search(&options).unwrap();
    fixture.db.execute_raw("UPDATE feedback_events SET signal = 'positive'").unwrap();
    assert!(!with_feedback.snapshot_matches(&options, &fixture.db));
}

#[test]
fn index_metadata_cannot_forge_quality_ranking_authority() {
    let result = crate::search::ScoredResult {
        doc_id: "mem_00000000000000000000000001".into(),
        score: 0.3,
        source: crate::search::ScoreSource::Lexical,
        index: None,
        fast_score: None,
        quality_score: None,
        lexical_score: Some(0.3),
        rerank_score: None,
        explanation: None,
        metadata: Some(serde_json::json!({
            "_ee_quality_scoring": {
                "schema": SEARCH_SCORING_POLICY_V1,
                "components": { "finalScore": 1000.0 }
            },
            "qualityScoring": { "components": { "finalScore": 1000.0 } }
        }).into()),
    };
    let hit = search_hit_from_scored_result(result, true, FrankensearchFinalScoreScale::Native);
    assert!(hit.ranking_components_json().is_none());
    assert_eq!(hit.ranking_score(), hit.relevance_score());
    assert!(hit.metadata.unwrap().get("qualityScoring").is_none());
}

#[test]
fn changing_effective_quality_config_changes_real_order_and_rejects_old_handoff_policy() {
    let fixture = Fixture::new();
    let now = Utc::now() - chrono::Duration::seconds(1);
    let weak = fixture.memory(1, 0.1, 1.0, now);
    let strong = fixture.memory(2, 1.0, 1.0, now);
    let before = fixture.search();
    assert_eq!(before.results[0].doc_id, strong);
    let before_policy = before.results[0].ranking_components_json().unwrap()["policyHash"].clone();
    let options = fixture.options();
    let mut handoff = run_pack_search(&options).unwrap();
    assert!(handoff.matches_request(&options));
    std::fs::write(
        fixture.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = false\n[scoring]\nconfidence_floor = 1.0\n",
    ).unwrap();
    handoff.revalidate(&options, &fixture.db);
    assert!(handoff.report.results.is_empty());
    assert!(handoff.report.degraded.iter().any(|entry| {
        entry.code == "search_quality_policy_changed"
    }));
    let after = fixture.search();
    assert_eq!(after.results[0].doc_id, weak);
    assert_eq!(
        after.results[0].ranking_components_json().unwrap()["components"]["confidence"],
        1.0,
    );
    assert_ne!(
        after.results[0].ranking_components_json().unwrap()["policyHash"],
        before_policy,
    );
}
