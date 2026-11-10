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
        self.memory_with_kind(number, confidence, utility, created, content, "note")
    }

    fn memory_with_kind(
        &self,
        number: u128,
        confidence: f32,
        utility: f32,
        created: DateTime<Utc>,
        content: &str,
        kind: &str,
    ) -> String {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: self.workspace.clone(),
                    content: content.to_owned(),
                    level: "semantic".to_owned(),
                    kind: kind.to_owned(),
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
        self.feedback("memory", id, number, "harmful");
    }

    fn feedback(&self, target_type: &str, id: &str, number: u128, signal: &str) {
        self.db
            .insert_feedback_event(
                &format!("fb_{number:026}"),
                &CreateFeedbackEventInput {
                    workspace_id: self.workspace.clone(),
                    target_type: target_type.to_owned(),
                    target_id: id.to_owned(),
                    signal: signal.to_owned(),
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

    fn evidence(&self, number: u128, content: &str) -> String {
        let session =
            crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_session(
                &session,
                &crate::db::CreateSessionInput {
                    workspace_id: self.workspace.clone(),
                    cass_session_id: format!("quality-evidence-session-{number}"),
                    source_path: None,
                    agent_name: Some("codex".to_owned()),
                    model: None,
                    started_at: None,
                    ended_at: None,
                    message_count: 1,
                    token_count: None,
                    content_hash: format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex()),
                    metadata_json: None,
                },
            )
            .unwrap();
        let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_evidence_span(
                &id,
                &crate::db::CreateEvidenceSpanInput {
                    workspace_id: self.workspace.clone(),
                    session_id: session,
                    memory_id: None,
                    producer_kind: crate::db::EvidenceProducerKind::CassImport,
                    cass_span_id: format!("quality-evidence-span-{number}"),
                    span_kind: "message".to_owned(),
                    start_line: 1,
                    end_line: 1,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: content.to_owned(),
                    content_hash: format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex()),
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .unwrap();
        id
    }

    fn search(&self) -> SearchReport {
        let reader = DbConnection::open_file_read_only(self.root.join(".ee/ee.db")).unwrap();
        run_search_with_read_connection(&self.options(), &reader).unwrap()
    }

    fn publish_index(&self) -> crate::core::index::TestWorkspaceEmbedderStackGuard {
        let guard = crate::core::index::install_test_hash_workspace_embedder(&self.workspace);
        let report = crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
            workspace_path: self.root.clone(),
            database_path: Some(self.root.join(".ee/ee.db")),
            index_dir: None,
            dry_run: false,
        })
        .unwrap();
        assert_eq!(
            report.status,
            crate::core::index::IndexRebuildStatus::Success
        );
        guard
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
    let lexical = cached_index_status_for_search(&options, &index, None, None)
        .unwrap()
        .report;
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
            // The factor is f32 and the JSON component is f64, so 0.04 differs
            // by f32 rounding; hold them to the same 1e-6 bound as above.
            assert!(
                (f64::from(factor.value) - anchor_score).abs() < 1.0e-6,
                "anchorMatch factor {} vs scoring component {anchor_score}",
                factor.value
            );
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
fn evidence_outcomes_change_search_pack_and_handoff_ranking_without_rewriting_relevance() {
    let fixture = Fixture::new();
    let first = fixture.evidence(0x901, BODY);
    let second = fixture.evidence(0x902, BODY);
    fixture.feedback("evidence", &first, 0x901, "neutral");
    let _embedder = fixture.publish_index();
    let reference = Utc::now() + chrono::Duration::minutes(1);
    let mut options = fixture.options();
    options.as_of = Some(reference);
    let mut pack_options = native_handoff_pack_options(&fixture, reference);
    pack_options.max_results = Some(1);
    pack_options.relevance_floor = Some(0.0);
    pack_options.output_options.cache_json_response = true;
    let mut previous: Option<PackSearchHandoff> = None;
    let mut raw_relevance = None;
    for signal in ["neutral", "harmful", "helpful"] {
        fixture
            .db
            .execute_raw(&format!(
                "UPDATE feedback_events SET signal = '{signal}' WHERE target_type = 'evidence'"
            ))
            .unwrap();
        if let Some(old) = previous.take() {
            assert!(!old.snapshot_matches(&options, &fixture.db));
        }
        let reader = DbConnection::open_file_read_only(options.resolve_database_path()).unwrap();
        let report = run_search_with_read_connection(&options, &reader).unwrap();
        assert_eq!(report.results.len(), 2);
        let expected = if signal == "harmful" { &second } else { &first };
        assert_eq!(&report.results[0].doc_id, expected);
        for hit in &report.results {
            let scoring = hit.ranking_components_json().unwrap();
            assert_eq!(scoring["entityType"], "evidence");
            assert_eq!(scoring["observedSignals"]["harmfulPenalty"], true);
            assert_eq!(
                scoring["inputs"]["harmfulCount"],
                usize::from(signal == "harmful" && hit.doc_id == first)
            );
            for unobserved in ["recency", "confidence", "utility", "maturity", "scopeMatch"] {
                assert_eq!(scoring["observedSignals"][unobserved], false);
            }
            if let Some(raw) = raw_relevance {
                assert_eq!(hit.relevance_score(), raw);
            } else {
                raw_relevance = Some(hit.relevance_score());
            }
        }
        let first_hit = report
            .results
            .iter()
            .find(|hit| hit.doc_id == first)
            .unwrap();
        let second_hit = report
            .results
            .iter()
            .find(|hit| hit.doc_id == second)
            .unwrap();
        assert_eq!(first_hit.score, second_hit.score);
        assert_eq!(first_hit.lexical_score, second_hit.lexical_score);
        assert_eq!(first_hit.relevance_score(), second_hit.relevance_score());
        if signal == "harmful" {
            assert!(first_hit.ranking_relevance_score() < second_hit.ranking_relevance_score());
        } else {
            assert_eq!(
                first_hit.ranking_relevance_score(),
                second_hit.ranking_relevance_score()
            );
        }
        let mut limited = options.clone();
        limited.limit = 1;
        let mut handoff = run_pack_search(&limited).unwrap();
        assert_eq!(handoff.report.results.len(), 1);
        assert_eq!(&handoff.report.results[0].doc_id, expected);
        handoff.revalidate(&limited, &reader);
        assert_eq!(&handoff.report.results[0].doc_id, expected);
        assert_eq!(
            run_diag_search(&limited).unwrap().final_report.results[0].doc_id,
            *expected,
        );
        let canonical = crate::core::context::run_context_pack(&pack_options).unwrap();
        assert_eq!(canonical.data.pack.evidence_items.len(), 1);
        assert_eq!(&canonical.data.pack.evidence_items[0].evidence_id, expected);
        assert_eq!(canonical.data.pack.evidence_items[0].content, BODY);
        assert_eq!(
            canonical.data.pack.evidence_items[0].relevance.into_inner(),
            report.results[0].ranking_relevance_score(),
        );
        let provider = |search_options: &SearchOptions, _: bool| {
            run_pack_search(search_options)
                .map_err(|error| SearchDegradation::daemon_fallback(&error.to_string()))
        };
        let remote = crate::core::context::run_context_pack_with_search_provider(
            &pack_options,
            "pack",
            &provider,
        )
        .unwrap();
        assert_eq!(remote.response.data.pack, canonical.data.pack);
        assert!(
            !remote
                .response
                .data
                .degraded
                .iter()
                .any(|entry| entry.code == "daemon_search_fallback")
        );
        previous = Some(run_pack_search(&options).unwrap());
    }
    assert_eq!(fixture.db.count_table_rows("pack_records").unwrap(), 0);
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
fn published_search_filters_matching_candidates_before_top_one_and_diagnostics() {
    let fixture = Fixture::new();
    let now = Utc::now() - chrono::Duration::seconds(1);
    let note = fixture.memory(1, 1.0, 1.0, now);
    let other_failure = fixture.memory_with_kind(2, 0.8, 1.0, now, BODY, "failure");
    let matching = fixture.memory_with_kind(3, 0.4, 1.0, now, BODY, "failure");
    fixture
        .db
        .set_memory_typed_fields_json(&other_failure, Some(r#"{"family":"other-cache"}"#))
        .unwrap();
    fixture
        .db
        .set_memory_typed_fields_json(&matching, Some(r#"{"family":"target-cache"}"#))
        .unwrap();
    let mut options = fixture.options();
    options.limit = 1;
    let _embedder = fixture.publish_index();
    fixture.db.close().unwrap();

    // The wrong-kind note ranks first; the wrong-field failure ranks ahead
    // of the matching failure. Both filters must refill from the larger
    // retrieval pool instead of removing the already-truncated top result.
    for (kind, fields, expected, considered) in [
        (None, Vec::new(), Some(note.as_str()), 3),
        (Some("failure"), Vec::new(), Some(other_failure.as_str()), 2),
        (
            None,
            vec![TypedMemoryFieldFilter::parse("family=target-cache").unwrap()],
            Some(matching.as_str()),
            1,
        ),
        (
            Some("failure"),
            vec![TypedMemoryFieldFilter::parse("family=target-cache").unwrap()],
            Some(matching.as_str()),
            1,
        ),
        (Some("decision"), Vec::new(), None, 0),
        (
            Some("failure"),
            vec![TypedMemoryFieldFilter::parse("family=missing-cache").unwrap()],
            None,
            0,
        ),
    ] {
        let run = run_search_with_performance_and_filters(&options, kind, &fields).unwrap();
        assert_eq!(
            run.report
                .results
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            expected.into_iter().collect::<Vec<_>>()
        );
        let audit = run.audit_facts.as_ref().unwrap();
        assert_eq!(audit.kept, run.report.results.len());
        assert_eq!(audit.floor_counts.considered, considered);
        assert_eq!(audit.floor_counts.passed, considered);
        assert_eq!(run.report.candidates_below_floor, 0);
        assert_eq!(
            run.report.status,
            if expected.is_some() {
                SearchStatus::Success
            } else {
                SearchStatus::NoResults
            }
        );
        if expected.is_none() {
            let assist = run.report.query_assist.as_ref().unwrap();
            assert_eq!(assist.candidate_count, 0);
            assert!(assist.did_you_mean.is_empty());
            assert!(run.report.degraded.iter().all(|entry| !matches!(
                entry.code.as_str(),
                "no_relevant_results" | "low_recall_after_floor"
            )));
        }
    }
}

#[test]
fn empty_search_with_memory_filters_keeps_empty_result_diagnostics() {
    let fixture = Fixture::new();
    let options = fixture.options();
    let _embedder = fixture.publish_index();
    fixture.db.close().unwrap();
    let fields = [TypedMemoryFieldFilter::parse("family=target-cache").unwrap()];
    let run = run_search_with_performance_and_filters(&options, Some("failure"), &fields).unwrap();
    assert_eq!(run.report.status, SearchStatus::NoResults);
    assert!(run.report.results.is_empty());
    assert_eq!(run.audit_facts.as_ref().unwrap().kept, 0);
    assert_eq!(run.report.query_assist.as_ref().unwrap().candidate_count, 0);
}

#[test]
fn global_memory_filters_use_the_global_snapshot_before_top_one() {
    const GLOBAL_CHILD: &str = "EE_TEST_GLOBAL_TYPED_SEARCH_FILTERS";
    if std::env::var_os(GLOBAL_CHILD).is_some() {
        assert_global_memory_filters_use_the_global_snapshot();
        return;
    }
    let global_root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("core::search::quality_tests::global_memory_filters_use_the_global_snapshot_before_top_one")
        .arg("--nocapture")
        .env(GLOBAL_CHILD, "1")
        .env("XDG_DATA_HOME", global_root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "global filtered search child failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

fn assert_global_memory_filters_use_the_global_snapshot() {
    let fixture = Fixture::new();
    fixture.memory(1, 1.0, 1.0, Utc::now());
    let _local_embedder = fixture.publish_index();
    std::fs::write(
        fixture.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = true\n",
    )
    .unwrap();
    let paths = crate::core::global_store::default_global_store_paths_from_env().unwrap();
    let (global, workspace) =
        crate::core::global_store::open_or_create_global_store(&paths).unwrap();
    let mut ids = Vec::new();
    for (number, kind, confidence, fields) in [
        (2, "note", 1.0, None),
        (3, "failure", 0.8, Some(r#"{"family":"other-cache"}"#)),
        (4, "failure", 0.4, Some(r#"{"family":"target-cache"}"#)),
    ] {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        global
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: workspace.clone(),
                    content: BODY.to_owned(),
                    level: "semantic".to_owned(),
                    kind: kind.to_owned(),
                    workflow_id: None,
                    confidence,
                    utility: 1.0,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    provenance_uri: Some("manual://global-typed-filters".to_owned()),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .unwrap();
        global.set_memory_typed_fields_json(&id, fields).unwrap();
        assert!(fixture.db.get_memory(&id).unwrap().is_none());
        ids.push(id);
    }
    let fields = [TypedMemoryFieldFilter::parse("family=target-cache").unwrap()];
    let mut options = fixture.options();
    options.limit = 1;
    let search = |scope, kind, fields: &[TypedMemoryFieldFilter]| {
        let mut options = options.clone();
        options.memory_scope = scope;
        // Exercise the canonical held-snapshot path without reconciliation,
        // keeping both unpublished and published global retrieval observable.
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            run_search_with_performance_and_filters_with_cx_and_reconcile_timeout(
                &cx,
                &options,
                kind,
                fields,
                Duration::ZERO,
                Duration::from_secs(30),
                None,
                true,
                false,
            )
            .await
        })
        .unwrap()
        .unwrap()
    };
    let _global_embedder = crate::core::index::install_test_hash_workspace_embedder(&workspace);
    for published in [false, true] {
        if published {
            let rebuilt =
                crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
                    workspace_path: paths.root.clone(),
                    database_path: Some(paths.database_path.clone()),
                    index_dir: Some(paths.index_dir.clone()),
                    dry_run: false,
                })
                .unwrap();
            assert_eq!(
                rebuilt.status,
                crate::core::index::IndexRebuildStatus::Success
            );
        }
        for scope in [MemoryScope::Workspace, MemoryScope::Global] {
            let unfiltered = search(scope, None, &[]);
            assert_eq!(unfiltered.report.results.len(), 1);
            assert_eq!(
                unfiltered.report.results[0].metadata.as_ref().unwrap()["kind"],
                "note"
            );
            let kind_only = search(scope, Some("failure"), &[]);
            assert_eq!(kind_only.report.results.len(), 1);
            assert_eq!(kind_only.report.results[0].doc_id, ids[1]);
            let matching = search(scope, Some("failure"), &fields);
            assert_eq!(matching.report.status, SearchStatus::Success);
            assert_eq!(matching.report.results.len(), 1);
            assert_eq!(matching.report.results[0].doc_id, ids[2]);
            assert_eq!(matching.audit_facts.as_ref().unwrap().kept, 1);
            assert_eq!(
                matching
                    .audit_facts
                    .as_ref()
                    .unwrap()
                    .floor_counts
                    .considered,
                1
            );
            assert_eq!(
                matching.report.results[0].metadata.as_ref().unwrap()["storeLane"],
                crate::core::global_store::GLOBAL_PROVENANCE_LANE
            );
            let missing = search(scope, Some("decision"), &[]);
            assert_eq!(missing.report.status, SearchStatus::NoResults);
            assert!(missing.report.results.is_empty());
            assert!(missing.report.query_assist.unwrap().did_you_mean.is_empty());
        }
    }

    std::fs::write(
        fixture.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = false\n",
    )
    .unwrap();
    let disabled = search(MemoryScope::Workspace, Some("failure"), &fields);
    assert!(disabled.report.results.is_empty());
    assert!(
        disabled
            .report
            .degraded
            .iter()
            .any(|entry| entry.code == "global_lane_disabled")
    );

    // Change the sidecar after the helper has read/admitted the memory body.
    // Filtering must still see the old fields in that same global snapshot.
    let admitted = crate::core::global_store::read_global_store_memories_filtered_at(
        &paths,
        false,
        None,
        |connection, memory| {
            if memory.id != ids[2] {
                return Ok(false);
            }
            global
                .set_memory_typed_fields_json(&memory.id, Some(r#"{"family":"new-cache"}"#))
                .map_err(|error| error.to_string())?;
            typed_memory_matches(connection, memory, Some("failure"), &fields)
                .map_err(|error| error.to_string())
        },
    )
    .unwrap();
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].id, ids[2]);
    assert!(
        global
            .get_memory_typed_fields_json(&ids[2])
            .unwrap()
            .unwrap()
            .contains("new-cache")
    );
    let error = crate::core::global_store::read_global_store_memories_filtered_at(
        &paths,
        false,
        None,
        |_, _| Err("PRIVATE-FILTER-READ-CANARY".to_owned()),
    )
    .unwrap_err();
    assert!(error.contains("optional global lane was withheld"));
    assert!(!error.contains("PRIVATE-FILTER-READ-CANARY"));
}

#[test]
fn global_filter_collisions_obey_local_authority_before_top_one() {
    const GLOBAL_CHILD: &str = "EE_TEST_GLOBAL_FILTER_COLLISIONS";
    if std::env::var_os(GLOBAL_CHILD).is_some() {
        assert_global_filter_collisions_obey_local_authority();
        return;
    }
    let global_root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("core::search::quality_tests::global_filter_collisions_obey_local_authority_before_top_one")
        .arg("--nocapture")
        .env(GLOBAL_CHILD, "1")
        .env("XDG_DATA_HOME", global_root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "global collision search child failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

fn assert_global_filter_collisions_obey_local_authority() {
    let fixture = Fixture::new();
    let now = Utc::now();
    let wrong_kind = fixture.memory(101, 1.0, 1.0, now);
    let wrong_field = fixture.memory_with_kind(102, 0.9, 1.0, now, BODY, "failure");
    fixture
        .db
        .set_memory_typed_fields_json(&wrong_field, Some(r#"{"family":"other-cache"}"#))
        .unwrap();
    let matching_local = fixture.memory_with_kind(104, 0.2, 1.0, now, BODY, "failure");
    fixture
        .db
        .set_memory_typed_fields_json(&matching_local, Some(r#"{"family":"kept-local"}"#))
        .unwrap();
    let _local_embedder = fixture.publish_index();
    std::fs::write(
        fixture.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = true\n",
    )
    .unwrap();
    let paths = crate::core::global_store::default_global_store_paths_from_env().unwrap();
    let (global, workspace) =
        crate::core::global_store::open_or_create_global_store(&paths).unwrap();
    let replacement = MemoryId::from_uuid(uuid::Uuid::from_u128(103)).to_string();
    for (id, confidence, fields) in [
        (&wrong_kind, 1.0, r#"{"family":"target-cache"}"#),
        (&wrong_field, 0.9, r#"{"family":"target-cache"}"#),
        (&replacement, 0.4, r#"{"family":"target-cache"}"#),
        (&matching_local, 0.2, r#"{"family":"kept-local"}"#),
    ] {
        global
            .insert_memory(
                id,
                &CreateMemoryInput {
                    workspace_id: workspace.clone(),
                    content: BODY.to_owned(),
                    level: "semantic".to_owned(),
                    kind: "failure".to_owned(),
                    workflow_id: None,
                    confidence,
                    utility: 1.0,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    provenance_uri: Some("manual://global-filter-collision".to_owned()),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .unwrap();
        global
            .set_memory_typed_fields_json(id, Some(fields))
            .unwrap();
    }
    let mut options = fixture.options();
    options.limit = 1;
    let search = |kind, fields: &[TypedMemoryFieldFilter]| {
        let options = &options;
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            run_search_with_performance_and_filters_with_cx_and_reconcile_timeout(
                &cx,
                options,
                kind,
                fields,
                Duration::ZERO,
                Duration::from_secs(30),
                None,
                true,
                false,
            )
            .await
        })
        .unwrap()
        .unwrap()
    };
    let _global_embedder = crate::core::index::install_test_hash_workspace_embedder(&workspace);
    for published in [false, true] {
        if published {
            crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
                workspace_path: paths.root.clone(),
                database_path: Some(paths.database_path.clone()),
                index_dir: Some(paths.index_dir.clone()),
                dry_run: false,
            })
            .unwrap();
        }
        let unfiltered = search(None, &[]);
        assert_eq!(unfiltered.report.results[0].doc_id, wrong_kind);
        assert_ne!(
            unfiltered.report.results[0].metadata.as_ref().unwrap()["storeLane"],
            crate::core::global_store::GLOBAL_PROVENANCE_LANE
        );
        let kind_only = search(Some("failure"), &[]);
        assert_eq!(kind_only.report.results.len(), 1);
        assert_eq!(kind_only.report.results[0].doc_id, wrong_field);

        let fields = [TypedMemoryFieldFilter::parse("family=target-cache").unwrap()];
        for kind in [None, Some("failure")] {
            let matching = search(kind, &fields);
            assert_eq!(matching.report.status, SearchStatus::Success);
            assert_eq!(matching.report.results.len(), 1);
            assert_eq!(matching.report.results[0].doc_id, replacement);
            assert_eq!(
                matching
                    .audit_facts
                    .as_ref()
                    .unwrap()
                    .floor_counts
                    .considered,
                1
            );
            assert_eq!(matching.report.candidates_below_floor, 0);
            assert_eq!(
                matching.report.results[0].metadata.as_ref().unwrap()["storeLane"],
                crate::core::global_store::GLOBAL_PROVENANCE_LANE
            );
            assert!(matching.report.query_assist.as_ref().is_none_or(|assist| {
                assist
                    .did_you_mean
                    .iter()
                    .all(|hit| hit.doc_id != wrong_kind && hit.doc_id != wrong_field)
            }));
        }
        let fields = [TypedMemoryFieldFilter::parse("family=kept-local").unwrap()];
        let matching = search(Some("failure"), &fields);
        assert_eq!(matching.report.results.len(), 1);
        assert_eq!(matching.report.results[0].doc_id, matching_local);
        assert_ne!(
            matching.report.results[0].metadata.as_ref().unwrap()["storeLane"],
            crate::core::global_store::GLOBAL_PROVENANCE_LANE
        );
    }
}

#[test]
fn global_memory_filter_admission_uses_the_pinned_local_source() {
    let fixture = Fixture::new();
    let id = fixture.memory_with_kind(111, 0.8, 1.0, Utc::now(), BODY, "failure");
    fixture
        .db
        .set_memory_typed_fields_json(&id, Some(r#"{"family":"target-cache"}"#))
        .unwrap();
    let mut global_only = fixture.db.get_memory(&id).unwrap().unwrap();
    global_only.id = MemoryId::from_uuid(uuid::Uuid::from_u128(112)).to_string();
    let candidates = vec![
        fixture.db.get_memory(&id).unwrap().unwrap(),
        global_only.clone(),
    ];
    let fields = [TypedMemoryFieldFilter::parse("family=target-cache").unwrap()];
    let filters = SearchMemoryFilters {
        kind: Some("failure"),
        fields: &fields,
    };
    let reader =
        DbConnection::open_file_read_only(fixture.options().resolve_database_path()).unwrap();
    reader.begin_read_snapshot().unwrap();
    assert!(
        reader
            .get_memory_typed_fields_json(&id)
            .unwrap()
            .unwrap()
            .contains("target-cache")
    );
    fixture
        .db
        .set_memory_typed_fields_json(&id, Some(r#"{"family":"other-cache"}"#))
        .unwrap();
    let mut pinned = candidates.clone();
    filters
        .retain_global_memories_with_local_authority(&reader, &mut pinned)
        .unwrap();
    assert_eq!(
        pinned.len(),
        2,
        "the local sidecar must use the held snapshot"
    );
    reader.rollback_read_snapshot().unwrap();
    let mut current = candidates.clone();
    filters
        .retain_global_memories_with_local_authority(&reader, &mut current)
        .unwrap();
    assert_eq!(
        current.iter().map(|memory| &memory.id).collect::<Vec<_>>(),
        vec![&global_only.id]
    );

    // Invalid typed data is not permission to reuse the global twin's fields.
    fixture
        .db
        .execute_raw(&format!(
            "UPDATE memories SET typed_fields_json = '{{\"family\":42}}' WHERE id = '{id}'"
        ))
        .unwrap();
    let mut malformed = candidates.clone();
    filters
        .retain_global_memories_with_local_authority(&reader, &mut malformed)
        .unwrap();
    assert_eq!(
        malformed
            .iter()
            .map(|memory| &memory.id)
            .collect::<Vec<_>>(),
        vec![&global_only.id]
    );

    fixture
        .db
        .execute_raw("ALTER TABLE memories RENAME TO unavailable_global_collision_sources")
        .unwrap();
    let mut unavailable = candidates.clone();
    assert!(
        filters
            .retain_global_memories_with_local_authority(&reader, &mut unavailable)
            .is_err()
    );
    // Unfiltered retrieval adds neither a new store dependency nor extra reads.
    SearchMemoryFilters::default()
        .retain_global_memories_with_local_authority(&reader, &mut unavailable)
        .unwrap();
    assert_eq!(unavailable.len(), candidates.len());
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
fn evidence_feedback_scoring_reuses_only_its_source_snapshot_and_preserves_unknown_signals() {
    let fixture = Fixture::new();
    let evidence = fixture.evidence(0x911, BODY);
    let zero = fixture.evidence(0x912, BODY);
    let reference = DateTime::parse_from_rfc3339("2026-09-28T12:00:00.123456789Z")
        .unwrap()
        .with_timezone(&Utc);
    fixture.feedback("evidence", &evidence, 0x911, "harmful");
    fixture
        .db
        .execute_raw("UPDATE feedback_events SET created_at = '2026-09-28T12:00:00.123456789Z'")
        .unwrap();
    // A different native target type cannot lend evidence an outcome, even
    // with an identical target string and a malformed matching timestamp.
    fixture.feedback("memory", &evidence, 0x912, "harmful");
    fixture.db.execute_raw(
        "UPDATE feedback_events SET created_at = 'invalid-other-target-time' WHERE target_type = 'memory'",
    ).unwrap();
    let hits = [&evidence, &zero]
        .into_iter()
        .map(|id| SearchHit {
            doc_id: id.clone(),
            score: 0.8,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(0.8),
            rerank_score: None,
            metadata: None,
            explanation: None,
        })
        .collect();
    let mut hits = apply_live_evidence_visibility(
        &fixture.options(),
        hits,
        &mut Vec::new(),
        Some(&fixture.db),
    );
    assert_eq!(hits.len(), 2);
    let missing = EvidenceId::from_uuid(uuid::Uuid::from_u128(0x913)).to_string();
    let mut unowned = hits[0].clone();
    unowned.doc_id = missing.clone();
    hits.push(unowned);

    let reader =
        DbConnection::open_file_read_only(fixture.options().resolve_database_path()).unwrap();
    reader.begin_read_snapshot().unwrap();
    let (_, feedback) = search_score_calibration_with_feedback(
        &fixture.root,
        fixture.options().database_path.as_deref(),
        Some(&reader),
    );
    let feedback = feedback.unwrap();
    let mut sql_hits = hits.clone();
    let mut sql_degraded = Vec::new();
    apply_quality_scoring(
        &fixture.options(),
        &mut sql_hits,
        SearchScoringConfig::default(),
        reference,
        Some(&reader),
        None,
        None,
        &mut sql_degraded,
    );
    let mut reused_hits = hits.clone();
    let mut reused_degraded = Vec::new();
    apply_quality_scoring_with_feedback(
        &fixture.options(),
        &mut reused_hits,
        SearchScoringConfig::default(),
        reference,
        Some(&reader),
        None,
        None,
        Some(&feedback),
        &mut reused_degraded,
    );
    assert_eq!(
        serde_json::to_value(&sql_hits).unwrap(),
        serde_json::to_value(&reused_hits).unwrap()
    );
    assert!(sql_degraded.is_empty());
    assert!(reused_degraded.is_empty());
    for (id, expected) in [(&evidence, Some(1)), (&zero, Some(0)), (&missing, None)] {
        let scoring = reused_hits
            .iter()
            .find(|hit| &hit.doc_id == id)
            .unwrap()
            .ranking_components_json()
            .unwrap();
        assert_eq!(
            scoring["inputs"]["harmfulCount"],
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(
            scoring["observedSignals"]["harmfulPenalty"],
            expected.is_some()
        );
        assert_eq!(
            DateTime::parse_from_rfc3339(scoring["referenceTime"].as_str().unwrap())
                .unwrap()
                .with_timezone(&Utc),
            reference,
            "card expansion must receive the exact same accepted-outcome cutoff",
        );
    }
    let foreign = SearchFeedbackSnapshot {
        connection: &reader,
        workspace_id: "wsp_other".to_owned(),
        events: Vec::new(),
    };
    let mut foreign_hits = hits.clone();
    apply_quality_scoring_with_feedback(
        &fixture.options(),
        &mut foreign_hits,
        SearchScoringConfig::default(),
        reference,
        Some(&reader),
        None,
        None,
        Some(&foreign),
        &mut Vec::new(),
    );
    assert_eq!(
        serde_json::to_value(&foreign_hits).unwrap(),
        serde_json::to_value(&sql_hits).unwrap()
    );
    drop(foreign);

    fixture
        .db
        .execute_raw("UPDATE feedback_events SET signal = 'neutral' WHERE target_type = 'evidence'")
        .unwrap();
    let mut pinned_hits = hits.clone();
    apply_quality_scoring(
        &fixture.options(),
        &mut pinned_hits,
        SearchScoringConfig::default(),
        reference,
        Some(&reader),
        None,
        None,
        &mut Vec::new(),
    );
    assert_eq!(
        serde_json::to_value(&pinned_hits).unwrap(),
        serde_json::to_value(&sql_hits).unwrap()
    );
    let fresh =
        DbConnection::open_file_read_only(fixture.options().resolve_database_path()).unwrap();
    let mut fresh_hits = hits.clone();
    apply_quality_scoring_with_feedback(
        &fixture.options(),
        &mut fresh_hits,
        SearchScoringConfig::default(),
        reference,
        Some(&fresh),
        None,
        None,
        Some(&feedback),
        &mut Vec::new(),
    );
    assert_eq!(
        fresh_hits
            .iter()
            .find(|hit| hit.doc_id == evidence)
            .unwrap()
            .ranking_components_json()
            .unwrap()["inputs"]["harmfulCount"],
        0
    );
    drop(feedback);
    reader.rollback_read_snapshot().unwrap();

    fixture.db.execute_raw("UPDATE feedback_events SET signal = 'harmful', created_at = 'invalid-evidence-time' WHERE target_type = 'evidence'").unwrap();
    reader.begin_read_snapshot().unwrap();
    let (_, feedback) = search_score_calibration_with_feedback(
        &fixture.root,
        fixture.options().database_path.as_deref(),
        Some(&reader),
    );
    let feedback = feedback.unwrap();
    let mut unknown_hits = hits.clone();
    let mut unknown_degraded = Vec::new();
    apply_quality_scoring_with_feedback(
        &fixture.options(),
        &mut unknown_hits,
        SearchScoringConfig::default(),
        reference,
        Some(&reader),
        None,
        None,
        Some(&feedback),
        &mut unknown_degraded,
    );
    let mut unavailable_sql = hits;
    let mut unavailable_degraded = Vec::new();
    apply_quality_scoring(
        &fixture.options(),
        &mut unavailable_sql,
        SearchScoringConfig::default(),
        reference,
        Some(&reader),
        None,
        None,
        &mut unavailable_degraded,
    );
    assert_eq!(
        serde_json::to_value(&unknown_hits).unwrap(),
        serde_json::to_value(&unavailable_sql).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&unknown_degraded).unwrap(),
        serde_json::to_value(&unavailable_degraded).unwrap()
    );
    let scoring = unknown_hits
        .iter()
        .find(|hit| hit.doc_id == evidence)
        .unwrap()
        .ranking_components_json()
        .unwrap();
    assert!(scoring["inputs"]["harmfulCount"].is_null());
    assert_eq!(scoring["observedSignals"]["harmfulPenalty"], false);
    assert_eq!(scoring["components"]["harmfulPenalty"], 1.0);
    assert!(
        unknown_degraded
            .iter()
            .any(|entry| entry.code == "search_quality_signals_unavailable")
    );
    drop(feedback);
    reader.rollback_read_snapshot().unwrap();
}

#[test]
fn pinned_feedback_rows_match_sql_counts_and_quality_scores() {
    let fixture = Fixture::new();
    let reference = DateTime::parse_from_rfc3339("2026-09-28T12:00:00.123456789Z")
        .unwrap()
        .with_timezone(&Utc);
    let memory = fixture.memory(1, 1.0, 1.0, reference - chrono::Duration::days(1));
    let zero = fixture.memory(2, 1.0, 1.0, reference - chrono::Duration::days(1));
    let mut options = fixture.options();
    // The unpublished fixture supports current live retrieval, not historical
    // index selection. Seed source-owned hits independently of the runner's
    // clock, then apply the fixed instant only to the feedback/scoring oracle.
    options.include_future = true;
    let reader = DbConnection::open_file_read_only(options.resolve_database_path()).unwrap();
    let mut hits = run_search_with_read_connection(&options, &reader)
        .unwrap()
        .results;
    assert_eq!(hits.len(), 2);
    drop(reader);
    let missing = MemoryId::from_uuid(uuid::Uuid::from_u128(3)).to_string();
    let mut unowned = hits[0].clone();
    unowned.doc_id = missing.clone();
    hits.push(unowned);
    for (number, (target_type, target, signal, created_at)) in [
        ("memory", &memory, "negative", "2026-09-28T00:00:00Z"),
        ("memory", &memory, "harmful", "2026-09-28T02:00:00+02:00"),
        (
            "memory",
            &memory,
            "contradiction",
            "2026-09-28T12:00:00.123456789Z",
        ),
        (
            "memory",
            &memory,
            "inaccurate",
            "2026-09-28T12:00:00.123456789Z",
        ),
        (
            "memory",
            &memory,
            "negative",
            "2026-09-28T12:00:00.123456789Z",
        ),
        (
            "memory",
            &memory,
            "harmful",
            "2026-09-28T12:00:00.123456790Z",
        ),
        ("memory", &memory, "inaccurate", "2026-09-28T11:00:00-02:00"),
        ("memory", &memory, "negative", "2026-09-28T13:00:00+02:00"),
        ("memory", &memory, "positive", "unrelated nonharmful time"),
        ("memory", &zero, "neutral", "unrelated neutral time"),
        ("rule", &memory, "harmful", "unrelated rule time"),
        ("memory", &missing, "harmful", "unowned memory time"),
    ]
    .into_iter()
    .enumerate()
    {
        fixture
            .db
            .insert_feedback_event_for_recovery(&StoredFeedbackEvent {
                id: format!("fb_{number:026}"),
                workspace_id: fixture.workspace.clone(),
                target_type: target_type.to_owned(),
                target_id: target.clone(),
                signal: signal.to_owned(),
                weight: if number % 2 == 0 { 0.0 } else { 10.0 },
                source_type: "human_explicit".to_owned(),
                source_id: None,
                reason: None,
                evidence_json: None,
                session_id: None,
                applied_at: (number % 2 == 0).then(|| reference.to_rfc3339()),
                created_at: created_at.to_owned(),
            })
            .unwrap();
    }
    // The initial hit reader was dropped before seeding feedback. Open a new
    // source view so the pinned SQL and reused-row paths observe that ledger.
    let reader = DbConnection::open_file_read_only(options.resolve_database_path()).unwrap();
    reader.begin_read_snapshot().unwrap();
    let (_, feedback) = search_score_calibration_with_feedback(
        &fixture.root,
        fixture.options().database_path.as_deref(),
        Some(&reader),
    );
    let feedback = feedback.unwrap();
    let ids = [memory.as_str(), zero.as_str(), memory.as_str()];
    let expected = reader
        .memory_harmful_feedback_counts_at(&fixture.workspace, &ids, reference)
        .unwrap();
    assert_eq!(expected.get(&memory), Some(&6));
    assert_eq!(expected.get(&zero), Some(&0));
    assert_eq!(
        feedback.harmful_counts_at(&ids, reference).unwrap(),
        expected
    );

    let mut sql_hits = hits.clone();
    let mut sql_degraded = Vec::new();
    apply_quality_scoring(
        &fixture.options(),
        &mut sql_hits,
        SearchScoringConfig::default(),
        reference,
        Some(&reader),
        None,
        None,
        &mut sql_degraded,
    );
    let mut reused_degraded = Vec::new();
    apply_quality_scoring_with_feedback(
        &fixture.options(),
        &mut hits,
        SearchScoringConfig::default(),
        reference,
        Some(&reader),
        None,
        None,
        Some(&feedback),
        &mut reused_degraded,
    );
    assert_eq!(
        serde_json::to_value(&hits).unwrap(),
        serde_json::to_value(&sql_hits).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&reused_degraded).unwrap(),
        serde_json::to_value(&sql_degraded).unwrap()
    );
    let foreign_feedback = SearchFeedbackSnapshot {
        connection: &reader,
        workspace_id: "wsp_other".to_owned(),
        events: Vec::new(),
    };
    let mut foreign_hits = sql_hits.clone();
    apply_quality_scoring_with_feedback(
        &fixture.options(),
        &mut foreign_hits,
        SearchScoringConfig::default(),
        reference,
        Some(&reader),
        None,
        None,
        Some(&foreign_feedback),
        &mut Vec::new(),
    );
    assert_eq!(
        serde_json::to_value(&foreign_hits).unwrap(),
        serde_json::to_value(&sql_hits).unwrap(),
        "another workspace's ledger must not establish observed zeroes"
    );
    drop(foreign_feedback);
    let unowned = hits.iter().find(|hit| hit.doc_id == missing).unwrap();
    assert!(unowned.ranking_components_json().unwrap()["inputs"]["harmfulCount"].is_null());
    drop(feedback);
    reader.rollback_read_snapshot().unwrap();
}

#[test]
fn feedback_reuse_rejects_malformed_matching_time_and_different_source_views() {
    let fixture = Fixture::new();
    let reference = Utc::now() + chrono::Duration::minutes(1);
    let memory = fixture.memory(1, 1.0, 1.0, Utc::now());
    fixture.harmful(&memory, 1);
    let hits = fixture.search().results;
    let reader =
        DbConnection::open_file_read_only(fixture.options().resolve_database_path()).unwrap();
    reader.begin_read_snapshot().unwrap();
    let (_, feedback) = search_score_calibration_with_feedback(
        &fixture.root,
        fixture.options().database_path.as_deref(),
        Some(&reader),
    );
    let feedback = feedback.unwrap();
    fixture
        .db
        .execute_raw("UPDATE feedback_events SET signal = 'neutral'")
        .unwrap();
    let fresh =
        DbConnection::open_file_read_only(fixture.options().resolve_database_path()).unwrap();
    let mut fresh_hits = hits.clone();
    apply_quality_scoring_with_feedback(
        &fixture.options(),
        &mut fresh_hits,
        SearchScoringConfig::default(),
        reference,
        Some(&fresh),
        None,
        None,
        Some(&feedback),
        &mut Vec::new(),
    );
    assert_eq!(
        fresh_hits[0].ranking_components_json().unwrap()["inputs"]["harmfulCount"],
        0,
        "feedback from another connection must not replace a fresh authority read"
    );
    assert_eq!(
        feedback
            .harmful_counts_at(&[memory.as_str()], reference)
            .unwrap()
            .get(&memory),
        Some(&1)
    );
    drop(feedback);
    reader.rollback_read_snapshot().unwrap();

    fixture
        .db
        .execute_raw("UPDATE feedback_events SET signal = 'harmful', created_at = 'invalid-time'")
        .unwrap();
    reader.begin_read_snapshot().unwrap();
    let (_, feedback) = search_score_calibration_with_feedback(
        &fixture.root,
        fixture.options().database_path.as_deref(),
        Some(&reader),
    );
    let feedback = feedback.unwrap();
    assert!(
        reader
            .memory_harmful_feedback_counts_at(&fixture.workspace, &[memory.as_str()], reference)
            .is_err()
    );
    assert!(
        feedback
            .harmful_counts_at(&[memory.as_str()], reference)
            .is_err()
    );
    let mut reused_hits = hits.clone();
    let mut reused_degraded = Vec::new();
    apply_quality_scoring_with_feedback(
        &fixture.options(),
        &mut reused_hits,
        SearchScoringConfig::default(),
        reference,
        Some(&reader),
        None,
        None,
        Some(&feedback),
        &mut reused_degraded,
    );
    let mut sql_hits = hits;
    let mut sql_degraded = Vec::new();
    apply_quality_scoring(
        &fixture.options(),
        &mut sql_hits,
        SearchScoringConfig::default(),
        reference,
        Some(&reader),
        None,
        None,
        &mut sql_degraded,
    );
    assert_eq!(
        serde_json::to_value(&reused_hits).unwrap(),
        serde_json::to_value(&sql_hits).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&reused_degraded).unwrap(),
        serde_json::to_value(&sql_degraded).unwrap()
    );
    assert!(reused_hits[0].ranking_components_json().unwrap()["inputs"]["harmfulCount"].is_null());
    assert!(
        reused_degraded
            .iter()
            .any(|entry| entry.code == "search_quality_signals_unavailable")
    );
    drop(feedback);
    reader.rollback_read_snapshot().unwrap();
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
    assert!(with_feedback.snapshot_matches(&options, &fixture.db));
    let append_fingerprint = fixture
        .db
        .feedback_events_fingerprint(&fixture.workspace)
        .unwrap();
    fixture
        .db
        .execute_raw("UPDATE feedback_events SET signal = 'positive'")
        .unwrap();
    assert_eq!(
        fixture
            .db
            .feedback_events_fingerprint(&fixture.workspace)
            .unwrap(),
        append_fingerprint
    );
    assert!(!with_feedback.snapshot_matches(&options, &fixture.db));
}

#[test]
fn handoff_rejects_config_only_fusion_changes_with_unchanged_source_and_feedback() {
    let fixture = Fixture::new();
    let memory = fixture.memory(1, 1.0, 1.0, Utc::now());
    fixture.harmful(&memory, 1);
    let config_path = fixture.root.join(".ee/config.toml");
    let config = "[memory]\ninclude_global = false\n[search]\nlexical_weight = 0.95\nsemantic_weight = 0.05\n";
    std::fs::write(&config_path, config).unwrap();
    let options = fixture.options();
    let handoff = run_pack_search(&options).unwrap();
    assert!(handoff.snapshot_matches(&options, &fixture.db));
    assert_eq!(
        handoff.fusion_weights,
        resolved_search_fusion_weights(&fixture.root).unwrap()
    );
    let generation = fixture
        .db
        .get_workspace_generation(&fixture.workspace)
        .unwrap();
    let calibration_identity = handoff.snapshot.calibration_input_identity.clone();
    assert!(calibration_identity.is_some());

    // These same-size edits change ranking without changing any source rows,
    // feedback bytes or domain-quality policy. File metadata is not a witness.
    let modified = std::fs::metadata(&config_path).unwrap().modified().unwrap();
    std::fs::write(
        &config_path,
        "[memory]\ninclude_global = false\n[search]\nlexical_weight = 0.05\nsemantic_weight = 0.95\n",
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&config_path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(
        fixture
            .db
            .get_workspace_generation(&fixture.workspace)
            .unwrap(),
        generation
    );
    assert_eq!(
        search_score_calibration_for_workspace_cached(
            &fixture.root,
            options.database_path.as_deref(),
            Some(&fixture.db),
        )
        .input_identity(),
        calibration_identity
    );
    assert_eq!(
        resolved_search_scoring_config(&fixture.root)
            .unwrap()
            .policy_hash(),
        handoff.quality_scoring_policy
    );
    assert!(!handoff.matches_request(&options));
    assert!(!handoff.snapshot_matches(&options, &fixture.db));
    let refreshed = run_pack_search(&options).unwrap();
    assert!(refreshed.snapshot_matches(&options, &fixture.db));
    assert_ne!(refreshed.fusion_weights, handoff.fusion_weights);

    // An unrelated textual config edit with the same effective values remains
    // reusable; the handoff binds retrieval settings, not a whole-file mtime.
    std::fs::write(&config_path, format!("{config}# unrelated comment\n")).unwrap();
    assert!(handoff.snapshot_matches(&options, &fixture.db));
    assert_eq!(fixture.db.count_table_rows("pack_records").unwrap(), 0);
}

#[test]
fn handoff_rejects_config_only_rerank_mode_and_top_k_changes() {
    let fixture = Fixture::new();
    fixture.memory(1, 1.0, 1.0, Utc::now());
    let config_path = fixture.root.join(".ee/config.toml");
    std::fs::write(
        &config_path,
        "[memory]\ninclude_global = false\n[search]\nrerank = 'auto'\nrerank_top_k = 17\n",
    )
    .unwrap();
    let options = fixture.options();
    let handoff = run_pack_search(&options).unwrap();
    assert!(handoff.snapshot_matches(&options, &fixture.db));
    assert_eq!(
        handoff.report.rerank_configured_mode,
        crate::config::SearchRerankMode::Auto
    );
    assert_eq!(handoff.report.rerank_configured_top_k, 17);
    for (mode, top_k) in [("off", 17), ("auto", 19)] {
        std::fs::write(
            &config_path,
            format!("[memory]\ninclude_global = false\n[search]\nrerank = '{mode}'\nrerank_top_k = {top_k}\n"),
        )
        .unwrap();
        assert_eq!(
            resolved_search_fusion_weights(&fixture.root).unwrap(),
            handoff.fusion_weights
        );
        assert_eq!(
            resolved_search_scoring_config(&fixture.root)
                .unwrap()
                .policy_hash(),
            handoff.quality_scoring_policy
        );
        assert!(!handoff.matches_request(&options));
        assert!(!handoff.snapshot_matches(&options, &fixture.db));
        assert!(
            run_pack_search(&options)
                .unwrap()
                .snapshot_matches(&options, &fixture.db)
        );
    }
}

#[test]
fn feedback_handoff_preserves_canonical_quality_order_before_candidate_truncation() {
    let fixture = Fixture::new();
    let now = Utc::now() - chrono::Duration::minutes(1);
    let first = fixture.memory(1, 1.0, 1.0, now);
    let second = fixture.memory(2, 0.9, 1.0, now);
    fixture.harmful(&first, 1);
    fixture.harmful(&first, 2);
    let _embedder = fixture.publish_index();
    let mut options = fixture.options();
    options.limit = 1;
    options.as_of = Some(Utc::now() + chrono::Duration::minutes(1));
    let generation = fixture
        .db
        .get_workspace_generation(&fixture.workspace)
        .unwrap();
    let append_fingerprint = fixture
        .db
        .feedback_events_fingerprint(&fixture.workspace)
        .unwrap();
    let mut previous: Option<PackSearchHandoff> = None;
    for signal in ["neutral", "harmful", "positive", "harmful"] {
        fixture
            .db
            .execute_raw(&format!("UPDATE feedback_events SET signal = '{signal}'"))
            .unwrap();
        assert_eq!(
            fixture
                .db
                .get_workspace_generation(&fixture.workspace)
                .unwrap(),
            generation
        );
        assert_eq!(
            fixture
                .db
                .feedback_events_fingerprint(&fixture.workspace)
                .unwrap(),
            append_fingerprint
        );
        if let Some(old) = previous {
            assert!(
                !old.snapshot_matches(&options, &fixture.db),
                "a same-count edit must invalidate the old handoff"
            );
        }
        let mut handoff = run_pack_search(&options).unwrap();
        assert!(handoff.can_reuse_for_pack());
        assert!(handoff.snapshot_matches(&options, &fixture.db));
        let reader = DbConnection::open_file_read_only(options.resolve_database_path()).unwrap();
        let canonical = run_search_with_read_connection(&options, &reader).unwrap();
        assert_eq!(handoff.report.results.len(), 1);
        assert_eq!(
            handoff.report.results[0].doc_id,
            if signal == "harmful" { &second } else { &first }.as_str()
        );
        assert_eq!(
            serde_json::to_value(&handoff.report.results).unwrap(),
            serde_json::to_value(&canonical.results).unwrap()
        );
        handoff.revalidate(&options, &reader);
        assert_eq!(
            serde_json::to_value(&handoff.report.results).unwrap(),
            serde_json::to_value(&canonical.results).unwrap(),
            "client hydration must preserve the canonical scores and candidate order"
        );
        previous = Some(handoff);
    }
    assert_eq!(fixture.db.count_table_rows("pack_records").unwrap(), 0);
}

#[test]
fn feedback_calibration_edits_recompute_floor_admission_before_handoff() {
    let fixture = Fixture::new();
    let memory = fixture.memory(1, 1.0, 1.0, Utc::now());
    for number in 0..MIN_SEARCH_SCORE_CALIBRATION_SAMPLES {
        fixture.harmful(&memory, number as u128);
    }
    let _embedder = fixture.publish_index();
    let mut options = fixture.options();
    options.relevance_floor = Some(0.5);
    options.as_of = Some(Utc::now() + chrono::Duration::minutes(1));
    let append_fingerprint = fixture
        .db
        .feedback_events_fingerprint(&fixture.workspace)
        .unwrap();
    let mut previous: Option<PackSearchHandoff> = None;
    for truth in [0.1, 0.9, 0.1] {
        fixture.db.execute_raw(&format!(
            "UPDATE feedback_events SET signal = 'neutral', evidence_json = '{{\"score\":1.0,\"groundTruthRelevance\":{truth}}}'"
        )).unwrap();
        assert_eq!(
            fixture
                .db
                .feedback_events_fingerprint(&fixture.workspace)
                .unwrap(),
            append_fingerprint
        );
        if let Some(old) = previous {
            assert!(!old.snapshot_matches(&options, &fixture.db));
        }
        let handoff = run_pack_search(&options).unwrap();
        assert!(handoff.snapshot_matches(&options, &fixture.db));
        let reader = DbConnection::open_file_read_only(options.resolve_database_path()).unwrap();
        let canonical = run_search_with_read_connection(&options, &reader).unwrap();
        assert_eq!(
            serde_json::to_value(&handoff.report.results).unwrap(),
            serde_json::to_value(&canonical.results).unwrap()
        );
        assert_eq!(handoff.report.results.len(), usize::from(truth > 0.5));
        previous = Some(handoff);
    }
}

#[test]
fn feedback_handoff_is_bound_to_pinned_snapshot_during_concurrent_edits() {
    let fixture = Fixture::new();
    let memory = fixture.memory(1, 1.0, 1.0, Utc::now());
    fixture.harmful(&memory, 1);
    let _embedder = fixture.publish_index();
    let mut options = fixture.options();
    options.as_of = Some(Utc::now() + chrono::Duration::minutes(1));
    let mut handoff = run_pack_search(&options).unwrap();
    let pool = registered_process_read_pool(
        DatabaseConfig::file(options.resolve_database_path()),
        PoolConfig::default_single(),
    );
    let snapshot = pool.pin_snapshot().unwrap();
    let reader = snapshot.checked_connection().unwrap();
    assert!(handoff.snapshot_matches(&options, reader));
    let feedback = handoff.validate_snapshot(&options, reader).unwrap();
    let before = serde_json::to_value(&handoff.report.results).unwrap();
    let database = options.resolve_database_path();
    std::thread::spawn(move || {
        let writer = DbConnection::open_file(database).unwrap();
        writer
            .execute_raw("UPDATE feedback_events SET signal = 'positive'")
            .unwrap();
    })
    .join()
    .unwrap();
    assert!(
        handoff.snapshot_matches(&options, reader),
        "assembly continues against its admitted source view"
    );
    handoff.revalidate_with_feedback(&options, reader, Some(&feedback));
    assert_eq!(
        serde_json::to_value(&handoff.report.results).unwrap(),
        before
    );
    drop(feedback);
    snapshot.commit().unwrap();
    let fresh = DbConnection::open_file_read_only(options.resolve_database_path()).unwrap();
    assert!(
        !handoff.snapshot_matches(&options, &fresh),
        "a subsequent request must see the new feedback"
    );
    assert!(
        run_pack_search(&options)
            .unwrap()
            .snapshot_matches(&options, &fresh)
    );
}

fn native_handoff_entities(fixture: &Fixture) -> (Vec<String>, String) {
    let rules = [1.0, 0.9, 0.1]
        .into_iter()
        .enumerate()
        .map(|(index, confidence)| {
            let id = crate::models::RuleId::from_uuid(uuid::Uuid::from_u128(201 + index as u128))
                .to_string();
            fixture
                .db
                .insert_procedural_rule(
                    &id,
                    &CreateProceduralRuleInput {
                        workspace_id: fixture.workspace.clone(),
                        content: format!("qualityquasar release checklist item {index}."),
                        confidence,
                        utility: 1.0,
                        importance: 0.5,
                        trust_class: "human_explicit".to_owned(),
                        scope: if index == 1 { "global" } else { "workspace" }.to_owned(),
                        scope_pattern: None,
                        maturity: "validated".to_owned(),
                        protected: false,
                        source_memory_ids: Vec::new(),
                        tags: Vec::new(),
                    },
                )
                .unwrap();
            id
        })
        .collect::<Vec<_>>();
    let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(204)).to_string();
    fixture
        .db
        .insert_session(
            &session,
            &crate::db::CreateSessionInput {
                workspace_id: fixture.workspace.clone(),
                cass_session_id: "native-handoff-session".to_owned(),
                source_path: None,
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: Some("2026-01-01T00:00:00Z".to_owned()),
                ended_at: Some("2026-01-01T01:00:00Z".to_owned()),
                message_count: 1,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(session.as_bytes()).to_hex()),
                metadata_json: None,
            },
        )
        .unwrap();
    let evidence = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(205)).to_string();
    let excerpt = "The qualityquasar release checklist review completed successfully.";
    fixture
        .db
        .insert_evidence_span(
            &evidence,
            &crate::db::CreateEvidenceSpanInput {
                workspace_id: fixture.workspace.clone(),
                session_id: session,
                memory_id: None,
                producer_kind: crate::db::EvidenceProducerKind::CassImport,
                cass_span_id: "native-handoff-span".to_owned(),
                span_kind: crate::cass::CassSpanKind::Message.as_str().to_owned(),
                start_line: 1,
                end_line: 1,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                excerpt: excerpt.to_owned(),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )
        .unwrap();
    (rules, evidence)
}

fn native_handoff_pack_options(
    fixture: &Fixture,
    reference_time: DateTime<Utc>,
) -> crate::core::context::ContextPackOptions {
    crate::core::context::ContextPackOptions {
        bead_id: None,
        task_paths: Vec::new(),
        workspace_path: fixture.root.clone(),
        database_path: Some(fixture.root.join(".ee/ee.db")),
        index_dir: None,
        query: "qualityquasar".to_owned(),
        speed: SpeedMode::Instant,
        source_mode: SearchSourceMode::LexicalOnly,
        strict_source_mode: true,
        filters: crate::models::QueryFilters::default(),
        profile: Some(ContextPackProfile::Balanced),
        max_tokens: Some(4000),
        candidate_pool: Some(16),
        max_results: Some(16),
        include_tombstoned: false,
        as_of: Some(reference_time),
        include_expired: false,
        include_future: false,
        include_stale: false,
        // Exercise the default calibrated-hit exemption from elbow admission.
        relevance_floor: None,
        redaction_level: crate::models::RedactionLevel::Minimal,
        memory_scope: MemoryScope::Workspace,
        strict_scope: false,
        ppr_weight: None,
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: crate::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
        task_lens: None,
        require_fresh_sentinels: false,
        output_options: Default::default(),
        persist_pack: false,
        baseline_write: None,
        no_lod: true,
    }
}

#[test]
fn handoff_native_rehydration_preserves_calibration_and_exact_pack_selection() {
    let fixture = Fixture::new();
    let (rules, evidence) = native_handoff_entities(&fixture);
    let _embedder = fixture.publish_index();
    let reference_time = Utc::now() + chrono::Duration::minutes(1);
    let pack_options = native_handoff_pack_options(&fixture, reference_time);
    let mut search_options = fixture.options();
    search_options.as_of = Some(reference_time);
    search_options.explain = false;
    search_options.strict_source_mode = true;
    let persistence_before =
        ["audit_log", "pack_records"].map(|table| fixture.db.count_table_rows(table).unwrap());

    let calibration_path = search_score_calibration_jsonl_path(&fixture.root);
    std::fs::create_dir_all(calibration_path.parent().unwrap()).unwrap();
    // Insufficient evidence is the negative control: the low-confidence rule
    // must still be excluded by the uncalibrated heuristic. A real calibration
    // then exempts the same native candidates without changing their scores.
    for sample_count in [1, MIN_SEARCH_SCORE_CALIBRATION_SAMPLES] {
        std::fs::write(
            &calibration_path,
            "{\"score\":0.8,\"groundTruthRelevance\":0.78}\n".repeat(sample_count),
        )
        .unwrap();
        let calibrated = sample_count >= MIN_SEARCH_SCORE_CALIBRATION_SAMPLES;
        for scope in [
            MemoryScope::Workspace,
            MemoryScope::Verified,
            MemoryScope::Global,
        ] {
            search_options.memory_scope = scope;
            let mut handoff = run_pack_search(&search_options).unwrap();
            assert!(handoff.can_reuse_for_pack());
            assert!(
                handoff
                    .report
                    .results
                    .iter()
                    .any(|hit| hit.doc_id == rules[1])
            );
            if scope == MemoryScope::Workspace {
                for id in rules.iter().chain(std::iter::once(&evidence)) {
                    let hit = handoff
                        .report
                        .results
                        .iter()
                        .find(|hit| &hit.doc_id == id)
                        .unwrap();
                    assert_eq!(hit.metadata.as_ref().unwrap()["calibrated"], calibrated);
                }
            }
            // Canonical scope admission can also replace native metadata.
            // Preserve exactly the annotations that survived it; this handoff
            // repair must not introduce a missing calibration or scope.
            let before_results = serde_json::to_value(&handoff.report.results).unwrap();
            let before_degraded = serde_json::to_value(&handoff.report.degraded).unwrap();
            let pool = registered_process_read_pool(
                DatabaseConfig::file(search_options.resolve_database_path()),
                PoolConfig::default_single(),
            );
            let snapshot = pool.pin_snapshot().unwrap();
            let reader = snapshot.checked_connection().unwrap();
            let validated = handoff.validate_snapshot(&search_options, reader).unwrap();
            let (_, queries) = crate::db::capture_query_sql_for_test(|| {
                handoff.revalidate_with_feedback(&search_options, reader, Some(&validated));
            });
            assert_eq!(
                serde_json::to_value(&handoff.report.results).unwrap(),
                before_results
            );
            assert_eq!(
                serde_json::to_value(&handoff.report.degraded).unwrap(),
                before_degraded
            );
            assert!(
                !queries.iter().any(|sql| sql.contains(
                    "FROM feedback_events WHERE workspace_id = ?1 ORDER BY created_at ASC, id ASC"
                )),
                "native rehydration must reuse the validated calibration and ledger"
            );
            drop(validated);
            snapshot.commit().unwrap();
        }

        let canonical =
            crate::core::context::run_context_pack_with_performance(&pack_options, "pack").unwrap();
        let calls = AtomicUsize::new(0);
        let provider = |options: &SearchOptions, require_cached_local: bool| {
            assert!(!require_cached_local);
            calls.fetch_add(1, Ordering::SeqCst);
            let handoff = run_pack_search(options)
                .map_err(|error| SearchDegradation::daemon_fallback(&error.to_string()))?;
            // Exercise the versioned typed payload used by the actual daemon.
            PackSearchHandoff::from_value(serde_json::to_value(handoff).unwrap())
                .map_err(|error| SearchDegradation::daemon_fallback(&error.to_string()))
        };
        let remote = crate::core::context::run_context_pack_with_search_provider(
            &pack_options,
            "pack",
            &provider,
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            !remote
                .response
                .data
                .degraded
                .iter()
                .any(|entry| entry.code == "daemon_search_fallback")
        );
        assert_eq!(remote.response.data.pack, canonical.response.data.pack);
        let selected = &canonical.response.data.pack.rule_items;
        assert!(selected.iter().any(|item| item.rule_id == rules[0]));
        assert_eq!(
            selected.iter().any(|item| item.rule_id == rules[2]),
            calibrated,
            "calibration must control the native rule's elbow exemption"
        );
        assert!(
            canonical
                .response
                .data
                .pack
                .evidence_items
                .iter()
                .any(|item| item.evidence_id == evidence)
        );
    }
    assert_eq!(
        ["audit_log", "pack_records"].map(|table| fixture.db.count_table_rows(table).unwrap()),
        persistence_before,
        "read-only local and daemon handoff paths must not persist packs or audits"
    );
}

#[test]
fn handoff_rejects_changed_calibration_bytes_and_incompatible_snapshot_contracts() {
    let fixture = Fixture::new();
    fixture.memory(1, 1.0, 1.0, Utc::now());
    let options = fixture.options();
    let handoff = run_pack_search(&options).unwrap();
    assert!(handoff.snapshot_matches(&options, &fixture.db));
    let path = search_score_calibration_jsonl_path(&fixture.root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "{\"score\":1.0,\"groundTruthRelevance\":0.9}\n"
            .repeat(MIN_SEARCH_SCORE_CALIBRATION_SAMPLES),
    )
    .unwrap();
    assert!(!handoff.snapshot_matches(&options, &fixture.db));
    let handoff = run_pack_search(&options).unwrap();
    assert!(handoff.snapshot_matches(&options, &fixture.db));
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(
        &path,
        "{\"score\":1.0,\"groundTruthRelevance\":0.1}\n"
            .repeat(MIN_SEARCH_SCORE_CALIBRATION_SAMPLES),
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert!(
        !handoff.snapshot_matches(&options, &fixture.db),
        "same-size same-mtime calibration replacement is not fresh"
    );

    let handoff = run_pack_search(&options).unwrap();
    assert!(handoff.snapshot_matches(&options, &fixture.db));
    let value = serde_json::to_value(&handoff).unwrap();
    let mut missing_fusion_witness = value.clone();
    missing_fusion_witness
        .as_object_mut()
        .unwrap()
        .remove("fusion_weights");
    assert!(PackSearchHandoff::from_value(missing_fusion_witness).is_err());
    let mut incomplete_fusion_witness = value.clone();
    incomplete_fusion_witness["fusion_weights"]
        .as_object_mut()
        .unwrap()
        .remove("semantic");
    assert!(PackSearchHandoff::from_value(incomplete_fusion_witness).is_err());
    let mut unknown_fusion_witness = value.clone();
    unknown_fusion_witness["fusion_weights"]["unrecognized_arm"] = serde_json::json!(0.1);
    assert!(PackSearchHandoff::from_value(unknown_fusion_witness).is_err());
    let mut old_contract = value.clone();
    old_contract["snapshot"]
        .as_object_mut()
        .unwrap()
        .remove("calibration_input_identity");
    old_contract["snapshot"]["quality_feedback_absent"] = serde_json::json!(true);
    assert!(PackSearchHandoff::from_value(old_contract).is_err());
    let mut unknown_contract = value.clone();
    unknown_contract["snapshot"]["unknown_revision"] = serde_json::json!(1);
    assert!(PackSearchHandoff::from_value(unknown_contract).is_err());
    let mut wrong_generation = value;
    wrong_generation["snapshot"]["generation"] = serde_json::json!(u64::MAX);
    let wrong_generation = PackSearchHandoff::from_value(wrong_generation).unwrap();
    assert!(!wrong_generation.snapshot_matches(&options, &fixture.db));
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
