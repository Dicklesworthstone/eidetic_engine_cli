#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::db::{
    CreateFeedbackEventInput, CreateMemoryInput, CreateProceduralRuleInput, CreateWorkspaceInput,
};
use crate::search::{Embedder, EmbedderStack, IndexBuilder, IndexableDocument};
use std::sync::atomic::{AtomicUsize, Ordering};

const BODY: &str = "qualityquasar release guidance preserves the authoritative source.";
const ANCHOR_MATCH_BODY: &str = "qualityquasar release guidance for `src/engine/cache/queue.rs` beside src/queue/cache/engine.rs.";
const ANCHOR_OTHER_BODY: &str = "qualityquasar release guidance for src/engine/cache/queue.rs beside `src/queue/cache/engine.rs`.";

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
        Self {
            _directory: directory,
            root,
            db,
            workspace,
        }
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
        self.memory_with_content(number, confidence, utility, created, BODY)
    }

    fn memory_with_content(
        &self,
        number: u128,
        confidence: f32,
        utility: f32,
        created: DateTime<Utc>,
        content: &str,
    ) -> String {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: self.workspace.clone(),
                    content: content.to_owned(),
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
            )
            .unwrap();
        let created = created.to_rfc3339();
        self.db.execute_raw(&format!(
            "UPDATE memories SET created_at = '{created}', updated_at = '{created}' WHERE id = '{id}'"
        )).unwrap();
        id
    }

    fn harmful(&self, id: &str, number: u128) {
        self.db
            .insert_feedback_event(
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
            )
            .unwrap();
    }

    fn search(&self) -> SearchReport {
        let reader = DbConnection::open_file_read_only(self.root.join(".ee/ee.db")).unwrap();
        run_search_with_read_connection(&self.options(), &reader).unwrap()
    }
}

struct SemanticIdentityFixture {
    hash: HashEmbedder,
    embeds: AtomicUsize,
}

impl SemanticIdentityFixture {
    fn new(dimension: usize) -> Self {
        Self {
            hash: HashEmbedder::new(dimension, frankensearch::HashAlgorithm::FnvModular),
            embeds: AtomicUsize::new(0),
        }
    }
}

impl Embedder for SemanticIdentityFixture {
    fn embed<'a>(
        &'a self,
        cx: &'a asupersync::Cx,
        text: &'a str,
    ) -> frankensearch::SearchFuture<'a, Vec<f32>> {
        self.embeds.fetch_add(1, Ordering::Relaxed);
        self.hash.embed(cx, text)
    }

    fn identity(
        &self,
    ) -> frankensearch::SearchResult<&frankensearch::core::generation::EmbeddingIdentityBundleV1>
    {
        self.hash.identity()
    }

    fn id(&self) -> &str {
        self.hash.id()
    }

    fn model_name(&self) -> &str {
        self.hash.model_name()
    }

    fn dimension(&self) -> usize {
        self.hash.dimension()
    }

    fn is_ready(&self) -> bool {
        true
    }

    fn is_semantic(&self) -> bool {
        true
    }

    fn category(&self) -> frankensearch::core::traits::ModelCategory {
        self.hash.category()
    }
}

fn lexical_identity_fixture() -> (Fixture, String, Arc<SemanticIdentityFixture>) {
    let fixture = Fixture::new();
    let memory = fixture.memory(1, 1.0, 1.0, Utc::now() - chrono::Duration::seconds(1));
    let index = fixture.options().resolve_index_dir();
    let stored = Arc::new(SemanticIdentityFixture::new(256));
    let stored_id = stored.id().to_owned();
    let documents = vec![IndexableDocument::new(memory.as_str(), BODY)];
    let generation = fixture
        .db
        .get_workspace_generation(&fixture.workspace)
        .unwrap()
        .unwrap();
    let build_index = index.clone();
    crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        IndexBuilder::new(&build_index)
            .with_embedder_stack(EmbedderStack::from_parts(stored, None))
            .add_documents(documents.clone())
            .build(&cx)
            .await
            .unwrap();
        crate::core::index::build_lexical_tier(&cx, &build_index, &documents)
            .await
            .unwrap();
        crate::core::index::write_memory_eval_index_metadata_for_generation(
            &build_index,
            generation,
            1,
        )
        .unwrap();
    })
    .unwrap();
    // Stamp the same semantic metadata that publication records for this
    // deterministic producer. The other producer is passed explicitly, so
    // this regression never mutates the process-global embedder or environment.
    let path = index.join("meta.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    metadata["storedModelId"] = serde_json::json!(stored_id);
    metadata["storedDimension"] = serde_json::json!(256);
    std::fs::write(path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    (fixture, memory, Arc::new(SemanticIdentityFixture::new(512)))
}

fn diagnostic_with_identity(
    options: &SearchOptions,
    embedder: Arc<dyn Embedder>,
) -> Result<SearchDiagnosticReport, SearchError> {
    crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        run_diag_search_with_cx_and_workspace_embedder(&cx, options, Some(embedder)).await
    })
    .unwrap()
}

#[test]
fn lexical_search_and_diagnostics_ignore_unrelated_semantic_identity() {
    let (fixture, memory, unrelated) = lexical_identity_fixture();
    let mut options = fixture.options();
    options.strict_source_mode = true;
    let index = options.resolve_index_dir();
    assert!(
        crate::core::index::validate_index_corpus_compatibility_with_embedder(
            &index,
            unrelated.as_ref()
        )
        .is_err(),
        "fixture must reproduce the semantic admission mismatch"
    );

    let report = run_search_with_embedder(&options, unrelated.clone()).unwrap();
    assert_eq!(report.status, SearchStatus::Success);
    assert_eq!(report.results[0].doc_id, memory);
    assert_eq!(report.source_mode_applied, SearchSourceMode::LexicalOnly);
    assert!(report.index_freshness.is_none());
    let diagnostic = diagnostic_with_identity(&options, unrelated.clone()).unwrap();
    assert_eq!(diagnostic.final_report.results[0].doc_id, memory);
    assert!(!diagnostic.pre_fusion.semantic_fast.available);
    assert!(
        !diagnostic
            .final_report
            .index_freshness
            .as_ref()
            .unwrap()
            .stale
    );
    for report in [&report, &diagnostic.final_report] {
        assert!(
            !report.degraded.iter().any(|entry| matches!(
                entry.code.as_str(),
                "index_corrupt" | "index_stale" | "index_incompatible"
            )),
            "{:#?}",
            report.degraded
        );
    }

    for mode in [SearchSourceMode::SemanticOnly, SearchSourceMode::Hybrid] {
        options.source_mode = mode;
        assert!(run_search_with_embedder(&options, unrelated.clone()).is_err());
        assert!(diagnostic_with_identity(&options, unrelated.clone()).is_err());
    }
    assert_eq!(unrelated.embeds.load(Ordering::Relaxed), 0);
}

#[test]
fn lexical_identity_isolation_also_applies_without_a_source_generation() {
    let (fixture, memory, unrelated) = lexical_identity_fixture();
    let mut options = fixture.options();
    options.index_dir = Some(options.resolve_index_dir());
    options.database_path = Some(fixture.root.join("unavailable-source.db"));
    let report = run_search_with_embedder(&options, unrelated.clone()).unwrap();
    assert_ne!(report.status, SearchStatus::IndexError);
    let diagnostic = diagnostic_with_identity(&options, unrelated.clone()).unwrap();
    assert!(
        diagnostic
            .pre_fusion
            .lexical
            .results
            .iter()
            .any(|hit| hit.doc_id == memory)
    );
    assert!(!diagnostic.pre_fusion.semantic_fast.available);
    assert_eq!(unrelated.embeds.load(Ordering::Relaxed), 0);
    assert!(!options.resolve_database_path().exists());
}

#[test]
fn lexical_status_neither_consumes_nor_replaces_semantic_readiness_cache() {
    let (fixture, _, unrelated) = lexical_identity_fixture();
    let options = fixture.options();
    let index = options.resolve_index_dir();
    let status_options = IndexStatusOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: options.database_path.clone(),
        index_dir: Some(index.clone()),
    };
    let semantic = crate::core::index::get_index_status_with_embedder(
        &status_options,
        Some(&fixture.db),
        false,
        unrelated.as_ref(),
    )
    .unwrap();
    assert_eq!(semantic.health, IndexHealth::Stale);
    let cache_key = IndexStatusCacheKey::from_search_options(&options, &index);
    let cache = SEARCH_INDEX_STATUS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    cache.lock().unwrap().insert(
        cache_key.clone(),
        CachedIndexStatus {
            // Other parallel tests may prune the shared TTL cache. Keep this
            // planted entry live through slow fixture I/O so expiry cannot pass
            // for policy isolation or make the assertion machine-speed dependent.
            checked_at: Instant::now() + Duration::from_secs(60),
            report: semantic,
        },
    );
    let lexical = cached_index_status_for_search(&options, &index, None, None).unwrap();
    assert_eq!(lexical.health, IndexHealth::Ready);
    assert!(lexical.embedding.is_none());
    assert!(lexical.last_check_error.is_none());
    assert_eq!(
        cache.lock().unwrap().get(&cache_key).unwrap().report.health,
        IndexHealth::Stale
    );
}

#[test]
fn lexical_identity_exemption_preserves_generation_and_security_admission() {
    let (fixture, _, unrelated) = lexical_identity_fixture();
    let options = fixture.options();
    let index = options.resolve_index_dir();
    let path = index.join("meta.json");
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let status_options = IndexStatusOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: options.database_path.clone(),
        index_dir: Some(index.clone()),
    };
    for field in [
        "schema",
        "corpusRevision",
        "evidenceSecurityPolicyEpoch",
        "tierDocumentCounts",
    ] {
        let mut invalid = original.clone();
        invalid[field] = serde_json::Value::Null;
        std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(
            run_search_with_embedder(&options, unrelated.clone()).is_err(),
            "{field}"
        );
        assert!(
            diagnostic_with_identity(&options, unrelated.clone()).is_err(),
            "{field}"
        );
        let status = crate::core::index::get_index_status_for_lexical_search(
            &status_options,
            Some(&fixture.db),
            false,
        )
        .unwrap();
        assert_ne!(status.health, IndexHealth::Ready, "{field}");
        assert!(status.last_check_error.is_some(), "{field}");
    }
    let mut future = original.clone();
    let source = original["sourceGeneration"].as_u64().unwrap();
    future["sourceGeneration"] = serde_json::json!(source + 1);
    future["generation"] = serde_json::json!(source + 1);
    std::fs::write(&path, serde_json::to_vec(&future).unwrap()).unwrap();
    let status = crate::core::index::get_index_status_for_lexical_search(
        &status_options,
        Some(&fixture.db),
        false,
    )
    .unwrap();
    assert_eq!(status.health, IndexHealth::Stale);
    assert!(
        status
            .last_check_error
            .as_deref()
            .unwrap()
            .contains("newer than database generation")
    );
    assert!(run_search_with_embedder(&options, unrelated.clone()).is_err());
    assert!(diagnostic_with_identity(&options, unrelated.clone()).is_err());

    std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    assert!(
        crate::core::index::validate_index_corpus_compatibility_for_lexical_search(&index).is_ok()
    );
    let vector = index.join("vector.fast.idx");
    let mut bytes = std::fs::read(&vector).unwrap();
    assert!(!bytes.is_empty());
    bytes[0] ^= 0xff;
    std::fs::write(vector, bytes).unwrap();
    assert!(run_search_with_embedder(&options, unrelated.clone()).is_err());
    assert!(diagnostic_with_identity(&options, unrelated.clone()).is_err());
    assert_eq!(unrelated.embeds.load(Ordering::Relaxed), 0);
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
            if changed == "recency" {
                now - chrono::Duration::days(30)
            } else {
                now
            },
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
        assert_eq!(
            report.results[0].lexical_score,
            report.results[1].lexical_score
        );
        assert_eq!(
            report.results[0].relevance_score(),
            report.results[1].relevance_score()
        );
        assert!(report.results[0].ranking_score() > report.results[1].ranking_score());
        let components = report.results[1].ranking_components_json().unwrap();
        assert_eq!(components["observedSignals"][changed], true);
        assert_eq!(components["observedSignals"]["graphCentrality"], false);
        assert!(components["inputs"]["graphCentrality"].is_null());
        assert_eq!(components["components"]["graphCentrality"], 1.0);
        assert!(
            report.results[1]
                .explanation
                .as_ref()
                .unwrap()
                .factors
                .iter()
                .any(|factor| factor.name == changed && factor.value < 1.0)
        );
        let wire = report.data_json();
        assert_eq!(
            wire["results"][0]["metadata"]["qualityScoring"]["schema"],
            SEARCH_SCORING_POLICY_V1,
        );
        assert!(
            wire["results"][0]["metadata"]
                .get(SEARCH_QUALITY_SCORING_KEY)
                .is_none()
        );
    }
}

#[test]
fn exact_artifact_anchors_rank_live_memories_and_explain_matches_and_neutral_controls() {
    let fixture = Fixture::new();
    let now = Utc::now() - chrono::Duration::seconds(1);
    let other = fixture.memory_with_content(1, 1.0, 1.0, now, ANCHOR_OTHER_BODY);
    let matching = fixture.memory_with_content(2, 1.0, 1.0, now, ANCHOR_MATCH_BODY);
    let reader = DbConnection::open_file_read_only(fixture.root.join(".ee/ee.db")).unwrap();
    for (query, has_anchors, has_match) in [
        ("qualityquasar", false, false),
        ("qualityquasar `src/cache/engine/queue.rs`", true, false),
        ("qualityquasar `src/engine/cache/queue.rs`", true, true),
    ] {
        let mut options = fixture.options();
        options.query = query.to_owned();
        let report = run_search_with_read_connection(&options, &reader).unwrap();
        assert_eq!(report.status, SearchStatus::Success);
        assert_eq!(report.results.len(), 2);
        // Text tokens and positions are identical; only the code-quoted path
        // differs. Only the exact admitted precision anchor breaks the tie.
        assert_eq!(
            report.results[0].lexical_score,
            report.results[1].lexical_score
        );
        assert_eq!(
            report.results[0].relevance_score(),
            report.results[1].relevance_score()
        );
        let expected_first = if has_match { &matching } else { &other };
        assert_eq!(report.results[0].doc_id, *expected_first);
        if has_match {
            assert!(report.results[0].ranking_score() > report.results[1].ranking_score());
            assert!(
                report.results[0].ranking_relevance_score()
                    > report.results[1].ranking_relevance_score()
            );
        } else {
            assert_eq!(
                report.results[0].ranking_score(),
                report.results[1].ranking_score()
            );
        }
        for hit in &report.results {
            let matched = has_match && hit.doc_id == matching;
            let scoring = hit.ranking_components_json().unwrap();
            assert_eq!(scoring["observedSignals"]["anchorMatch"], has_anchors);
            assert_eq!(
                scoring["inputs"]["anchorMatchExactMatches"],
                if has_anchors {
                    serde_json::json!(usize::from(matched))
                } else {
                    serde_json::Value::Null
                }
            );
            assert_eq!(
                scoring["inputs"]["anchorMatchSource"],
                if has_anchors {
                    serde_json::json!("live_memory_content")
                } else {
                    serde_json::Value::Null
                }
            );
            assert_eq!(
                scoring["inputs"]["anchorMatchCapped"],
                if has_anchors {
                    serde_json::json!(false)
                } else {
                    serde_json::Value::Null
                }
            );
            let anchor_score = scoring["components"]["anchorMatch"].as_f64().unwrap();
            assert!((anchor_score - if matched { 0.04 } else { 0.0 }).abs() < 1.0e-6);
            let factor = hit
                .explanation
                .as_ref()
                .unwrap()
                .factors
                .iter()
                .find(|factor| factor.name == "anchorMatch")
                .unwrap();
            assert_eq!(f64::from(factor.value), anchor_score);
            assert_eq!(
                factor.contribution,
                if has_anchors {
                    "source-derived quality ranking component"
                } else {
                    "neutral: signal unavailable or not observed"
                }
            );
            assert!(!scoring.to_string().contains("src/"));
            if !has_anchors {
                assert!(scoring["inputs"]["anchorMatch"].is_null());
                assert_eq!(
                    scoring["rankingBound"],
                    serde_json::json!(
                        SearchScoringConfig {
                            anchor_match_bias_cap: 0.0,
                            ..SearchScoringConfig::default()
                        }
                        .ranking_bound()
                    )
                );
            }
        }
        let wire = report.data_json();
        assert_eq!(
            wire["results"][0]["metadata"]["qualityScoring"]["observedSignals"]["anchorMatch"],
            has_anchors
        );
        assert!(
            wire["results"][0]["metadata"]
                .get(SEARCH_QUALITY_SCORING_KEY)
                .is_none()
        );
    }
}

#[test]
fn published_search_and_diagnostics_promote_exact_anchors_before_top_one() {
    let fixture = Fixture::new();
    let now = Utc::now() - chrono::Duration::seconds(1);
    fixture.memory_with_content(1, 1.0, 1.0, now, ANCHOR_OTHER_BODY);
    let matching = fixture.memory_with_content(2, 1.0, 1.0, now, ANCHOR_MATCH_BODY);
    let mut options = fixture.options();
    options.query = "qualityquasar `src/engine/cache/queue.rs`".to_owned();
    options.limit = 1;
    let _embedder = crate::core::index::install_test_hash_workspace_embedder(&fixture.workspace);
    fixture.db.close().unwrap();
    let rebuild = crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: options.database_path.clone(),
        index_dir: None,
        dry_run: false,
    })
    .unwrap();
    assert_eq!(
        rebuild.status,
        crate::core::index::IndexRebuildStatus::Success
    );
    let reader = DbConnection::open_file_read_only(fixture.root.join(".ee/ee.db")).unwrap();
    let report = run_search_with_read_connection(&options, &reader).unwrap();
    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].doc_id, matching);
    assert_eq!(
        report.results[0].ranking_components_json().unwrap()["inputs"]["anchorMatchExactMatches"],
        1
    );
    let diagnostic = run_diag_search(&options).unwrap();
    assert_eq!(diagnostic.final_report.results.len(), 1);
    assert_eq!(diagnostic.final_report.results[0].doc_id, matching);
}

#[test]
fn anchor_ranking_ignores_forged_metadata_and_unadmitted_preloads() {
    let fixture = Fixture::new();
    let now = Utc::now();
    let id = fixture.memory_with_content(1, 1.0, 1.0, now, ANCHOR_OTHER_BODY);
    let mut hits = fixture.search().results;
    let mut options = fixture.options();
    options.query = "qualityquasar `src/engine/cache/queue.rs`".to_owned();
    let query_anchors = crate::search::query::query_anchor_match_context(&options.query);
    hits[0].metadata = Some(serde_json::json!({
        "content": ANCHOR_MATCH_BODY,
        "anchors": query_anchors.anchors,
        "qualityScoring": {
            "observedSignals": { "anchorMatch": true },
            "components": { "anchorMatch": 1000.0, "finalScore": 1000.0 }
        }
    }));
    apply_quality_scoring(
        &options,
        &mut hits,
        SearchScoringConfig::default(),
        now,
        Some(&fixture.db),
        None,
        None,
        &mut Vec::new(),
    );
    let scoring = hits[0].ranking_components_json().unwrap();
    assert_eq!(scoring["observedSignals"]["anchorMatch"], true);
    assert_eq!(scoring["inputs"]["anchorMatchExactMatches"], 0);
    assert_eq!(scoring["components"]["anchorMatch"], 0.0);

    // A preload without the global admission proof cannot establish authority,
    // even if it contains the desired text. A failed live read stays neutral.
    let mut preloaded = fixture.db.get_memories_batch(&[id.as_str()]).unwrap();
    preloaded.get_mut(&id).unwrap().content = ANCHOR_MATCH_BODY.to_owned();
    apply_quality_scoring(
        &options,
        &mut hits,
        SearchScoringConfig::default(),
        now,
        None,
        Some(&preloaded),
        None,
        &mut Vec::new(),
    );
    let scoring = hits[0].ranking_components_json().unwrap();
    assert_eq!(scoring["observedSignals"]["anchorMatch"], false);
    assert!(scoring["inputs"]["anchorMatch"].is_null());
    assert!(scoring["inputs"]["anchorMatchExactMatches"].is_null());
    assert!(scoring["inputs"]["anchorMatchSource"].is_null());
    assert_eq!(scoring["components"]["anchorMatch"], 0.0);
}

#[test]
fn live_anchor_matches_respect_the_cap_and_a_disabled_bias() {
    let fixture = Fixture::new();
    let now = Utc::now() - chrono::Duration::seconds(1);
    let body = "qualityquasar `src/engine.rs` `EE_PACK_TRACE` `ee.response.v2`";
    fixture.memory_with_content(1, 1.0, 1.0, now, body);
    let mut options = fixture.options();
    options.query = body.to_owned();
    let reader = DbConnection::open_file_read_only(fixture.root.join(".ee/ee.db")).unwrap();
    let mut hits = run_search_with_read_connection(&options, &reader)
        .unwrap()
        .results;
    assert_eq!(hits.len(), 1);
    apply_quality_scoring(
        &options,
        &mut hits,
        SearchScoringConfig::default(),
        now,
        Some(&reader),
        None,
        None,
        &mut Vec::new(),
    );
    let boosted = hits[0].ranking_score();
    let scoring = hits[0].ranking_components_json().unwrap();
    assert_eq!(scoring["inputs"]["anchorMatchExactMatches"], 3);
    assert_eq!(scoring["inputs"]["anchorMatchCapped"], true);
    assert!((scoring["components"]["anchorMatch"].as_f64().unwrap() - 0.08).abs() < 1.0e-6);
    apply_quality_scoring(
        &options,
        &mut hits,
        SearchScoringConfig {
            anchor_match_bias_cap: 0.0,
            ..SearchScoringConfig::default()
        },
        now,
        Some(&reader),
        None,
        None,
        &mut Vec::new(),
    );
    let scoring = hits[0].ranking_components_json().unwrap();
    assert_eq!(scoring["observedSignals"]["anchorMatch"], true);
    assert_eq!(scoring["inputs"]["anchorMatchExactMatches"], 3);
    assert_eq!(scoring["inputs"]["anchorMatchCapped"], true);
    assert_eq!(scoring["components"]["anchorMatch"], 0.0);
    assert!((boosted - hits[0].ranking_score() - 0.08).abs() < 1.0e-6);
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
        fixture
            .db
            .insert_procedural_rule(
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
            )
            .unwrap();
        fixture.db.execute_raw(&format!(
            "UPDATE procedural_rules SET created_at = '2026-01-01T00:00:00Z', negative_feedback_count = {negative} WHERE id = '{id}'"
        )).unwrap();
    }
    let report = fixture.search();
    let expected: Vec<String> = [3_u128, 2, 1]
        .into_iter()
        .map(|number| crate::models::RuleId::from_uuid(uuid::Uuid::from_u128(number)).to_string())
        .collect();
    assert_eq!(
        report
            .results
            .iter()
            .map(|hit| hit.doc_id.clone())
            .collect::<Vec<_>>(),
        expected,
    );
    assert_eq!(
        report.results[2].ranking_components_json().unwrap()["components"]["maturity"],
        0.5,
    );
    assert_eq!(
        report.results[1].ranking_components_json().unwrap()["inputs"]["harmfulCount"],
        1,
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
    })
    .unwrap();
    assert_eq!(
        rebuild.status,
        crate::core::index::IndexRebuildStatus::Success
    );
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
    fixture
        .db
        .execute_raw("ALTER TABLE feedback_events RENAME TO unavailable_feedback_events")
        .unwrap();
    let mut degraded = Vec::new();
    apply_quality_scoring(
        &fixture.options(),
        &mut hits,
        SearchScoringConfig::default(),
        Utc::now(),
        Some(&fixture.db),
        None,
        None,
        &mut degraded,
    );
    let scoring = hits
        .iter()
        .find(|hit| hit.doc_id == id)
        .unwrap()
        .ranking_components_json()
        .unwrap();
    assert!(scoring["inputs"]["harmfulCount"].is_null());
    assert_eq!(scoring["observedSignals"]["harmfulPenalty"], false);
    assert_eq!(scoring["components"]["harmfulPenalty"], 1.0);
    assert!(
        degraded
            .iter()
            .any(|entry| entry.code == "search_quality_signals_unavailable")
    );
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
        &fixture.options(),
        &mut hits,
        SearchScoringConfig::default(),
        now,
        Some(&fixture.db),
        None,
        None,
        &mut Vec::new(),
    );
    assert_eq!(hits[0].doc_id, strong);
    assert!(hits[0].ranking_score() > 1.0 && hits[1].ranking_score() > 1.0);
    assert!(hits[0].ranking_relevance_score() > hits[1].ranking_relevance_score());
    assert!(
        hits.iter()
            .all(|hit| (0.0..=1.0).contains(&hit.ranking_relevance_score()))
    );
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
    fixture
        .db
        .execute_raw("UPDATE feedback_events SET signal = 'positive'")
        .unwrap();
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
        metadata: Some(
            serde_json::json!({
                "_ee_quality_scoring": {
                    "schema": SEARCH_SCORING_POLICY_V1,
                    "components": { "finalScore": 1000.0 }
                },
                "qualityScoring": { "components": { "finalScore": 1000.0 } }
            })
            .into(),
        ),
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
    )
    .unwrap();
    handoff.revalidate(&options, &fixture.db);
    assert!(handoff.report.results.is_empty());
    assert!(
        handoff
            .report
            .degraded
            .iter()
            .any(|entry| { entry.code == "search_quality_policy_changed" })
    );
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

#[test]
fn workspace_anchor_cap_controls_real_ranking_and_invalidates_cached_handoffs() {
    let fixture = Fixture::new();
    let now = Utc::now() - chrono::Duration::seconds(1);
    let other = fixture.memory_with_content(1, 1.0, 1.0, now, ANCHOR_OTHER_BODY);
    let matching = fixture.memory_with_content(2, 1.0, 1.0, now, ANCHOR_MATCH_BODY);
    let mut options = fixture.options();
    options.query = "qualityquasar `src/engine/cache/queue.rs`".to_owned();
    let reader = DbConnection::open_file_read_only(fixture.root.join(".ee/ee.db")).unwrap();
    let before = run_search_with_read_connection(&options, &reader).unwrap();
    assert_eq!(before.results[0].doc_id, matching);
    let before_policy = before.results[0].ranking_components_json().unwrap()["policyHash"].clone();
    let mut handoff = run_pack_search(&options).unwrap();
    assert!(handoff.matches_request(&options));

    std::fs::write(
        fixture.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = false\n[scoring]\nanchor_match_bias_cap = 0.0\n",
    )
    .unwrap();
    handoff.revalidate(&options, &reader);
    assert!(handoff.report.results.is_empty());
    assert!(
        handoff
            .report
            .degraded
            .iter()
            .any(|entry| entry.code == "search_quality_policy_changed")
    );
    let disabled = run_search_with_read_connection(&options, &reader).unwrap();
    assert_eq!(disabled.results[0].doc_id, other);
    assert_eq!(disabled.results[1].doc_id, matching);
    assert_eq!(
        disabled.results[0].ranking_score(),
        disabled.results[1].ranking_score()
    );
    let scoring = disabled.results[1].ranking_components_json().unwrap();
    assert_eq!(scoring["observedSignals"]["anchorMatch"], true);
    assert_eq!(scoring["inputs"]["anchorMatchExactMatches"], 1);
    assert_eq!(scoring["components"]["anchorMatch"], 0.0);
    assert_ne!(scoring["policyHash"], before_policy);

    std::fs::write(
        fixture.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = false\n[scoring]\nanchor_match_bias_cap = 0.02\n",
    )
    .unwrap();
    let limited = run_search_with_read_connection(&options, &reader).unwrap();
    assert_eq!(limited.results[0].doc_id, matching);
    let scoring = limited.results[0].ranking_components_json().unwrap();
    assert!((scoring["components"]["anchorMatch"].as_f64().unwrap() - 0.02).abs() < 1.0e-6);
    assert_eq!(scoring["inputs"]["anchorMatchCapped"], true);
    assert_ne!(scoring["policyHash"], before_policy);
    assert_ne!(
        scoring["policyHash"],
        disabled.results[1].ranking_components_json().unwrap()["policyHash"]
    );
    assert_eq!(
        limited.results[0].lexical_score,
        before.results[0].lexical_score
    );
    assert_eq!(
        limited.results[0].relevance_score(),
        before.results[0].relevance_score()
    );
}
