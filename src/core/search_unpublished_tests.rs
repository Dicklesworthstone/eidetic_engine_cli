#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::db::{
    CreateEvidenceSpanInput, CreateMemoryInput, CreateProceduralRuleInput, CreateSessionInput,
    CreateWorkspaceInput, EvidenceProducerKind,
};
use crate::models::{EvidenceId, RuleId, SessionId};
#[cfg(unix)]
use crate::search::{Embedder, EmbedderStack};
use sqlmodel_core::Value;

const WORD: &str = "unpublishedquasar";
const BODY: &str = "The unpublishedquasar release requires cargo fmt before tagging.";

struct Fixture {
    _root: tempfile::TempDir,
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
                path: root.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        Self {
            _root: directory,
            root,
            db,
            workspace,
        }
    }

    fn reader(&self) -> DbConnection {
        DbConnection::open_file_read_only(self.root.join(".ee/ee.db")).unwrap()
    }

    fn options(&self) -> SearchOptions {
        SearchOptions {
            workspace_path: self.root.clone(),
            database_path: Some(self.root.join(".ee/ee.db")),
            index_dir: None,
            query: WORD.to_owned(),
            limit: 16,
            speed: SpeedMode::Default,
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
            no_global: false,
            strict_scope: false,
        }
    }

    fn memory(&self, number: u128, body: &str) -> String {
        self.memory_with_kind(number, body, "fact")
    }

    #[cfg(unix)]
    fn remember_deferred(&self, content: &str) -> crate::core::memory::RememberMemoryReport {
        let database_path = self.root.join(".ee/ee.db");
        let outcome = crate::core::memory::remember_memory_with_controls(
            &crate::core::memory::RememberMemoryOptions {
                workspace_path: &self.root,
                database_path: Some(&database_path),
                content,
                workflow_id: None,
                level: "semantic",
                kind: "fact",
                tags: None,
                confidence: 1.0,
                source: Some("manual://deferred-lexical-read"),
                allow_secret_mention: false,
                valid_from: None,
                valid_to: None,
                dry_run: false,
                auto_link: false,
                propose_candidates: false,
            },
            &crate::core::memory::RememberWriteControls {
                reinforce: false,
                idempotency_key: None,
                defer_index_processing: true,
            },
        )
        .unwrap();
        let report = match outcome {
            crate::core::memory::RememberOutcome::Created(report) => Some(report),
            _ => None,
        }
        .expect("a first write without an idempotency key creates a memory");
        assert!(report.persisted);
        assert_eq!(report.index_status, "queued");
        assert!(report.index_job_id.is_some());
        *report
    }

    fn memory_with_kind(&self, number: u128, body: &str, kind: &str) -> String {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: self.workspace.clone(),
                    content: body.to_owned(),
                    level: "semantic".to_owned(),
                    kind: kind.to_owned(),
                    workflow_id: None,
                    confidence: 1.0,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    provenance_uri: Some("manual://unpublished-source".to_owned()),
                    tags: Vec::new(),
                    valid_from: Some("2000-01-01T00:00:00Z".to_owned()),
                    valid_to: None,
                },
            )
            .unwrap();
        id
    }

    fn rule(&self) -> String {
        self.rule_with_maturity(201, "candidate")
    }

    fn rule_with_maturity(&self, number: u128, maturity: &str) -> String {
        let id = RuleId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_procedural_rule(
                &id,
                &CreateProceduralRuleInput {
                    workspace_id: self.workspace.clone(),
                    content: BODY.to_owned(),
                    confidence: 1.0,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    scope: "global".to_owned(),
                    scope_pattern: None,
                    maturity: maturity.to_owned(),
                    protected: false,
                    source_memory_ids: Vec::new(),
                    tags: Vec::new(),
                },
            )
            .unwrap();
        id
    }

    fn evidence(&self) -> String {
        let session = SessionId::from_uuid(uuid::Uuid::from_u128(202)).to_string();
        self.db
            .insert_session(
                &session,
                &CreateSessionInput {
                    workspace_id: self.workspace.clone(),
                    cass_session_id: "unpublished-session".to_owned(),
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
        let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(203)).to_string();
        self.db
            .insert_evidence_span(
                &id,
                &CreateEvidenceSpanInput {
                    workspace_id: self.workspace.clone(),
                    session_id: session.clone(),
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: "unpublished-span".to_owned(),
                    span_kind: crate::cass::CassSpanKind::Message.as_str().to_owned(),
                    start_line: 1,
                    end_line: 1,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: BODY.to_owned(),
                    content_hash: format!("blake3:{}", blake3::hash(BODY.as_bytes()).to_hex()),
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .unwrap();
        assert!(
            self.db
                .get_evidence_span(&id)
                .unwrap()
                .unwrap()
                .is_direct_pack_admitted_for_session(
                    &self.workspace,
                    &self.db.get_session(&session).unwrap().unwrap()
                )
        );
        id
    }

    fn state(&self) -> Vec<Vec<Vec<(String, Value)>>> {
        source_state(&self.db)
    }

    fn pack_options(&self) -> crate::core::context::ContextPackOptions {
        crate::core::context::ContextPackOptions {
            bead_id: None,
            task_paths: Vec::new(),
            workspace_path: self.root.clone(),
            database_path: Some(self.root.join(".ee/ee.db")),
            index_dir: None,
            query: WORD.to_owned(),
            speed: SpeedMode::Default,
            source_mode: SearchSourceMode::LexicalOnly,
            strict_source_mode: false,
            filters: crate::models::QueryFilters::default(),
            profile: Some(ContextPackProfile::Balanced),
            max_tokens: Some(4000),
            candidate_pool: Some(64),
            max_results: Some(16),
            include_tombstoned: false,
            as_of: None,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            redaction_level: crate::models::RedactionLevel::Minimal,
            memory_scope: MemoryScope::Workspace,
            no_global: false,
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
}

fn source_state(db: &DbConnection) -> Vec<Vec<Vec<(String, Value)>>> {
    [
        "workspaces",
        "memories",
        "memory_tags",
        "memory_anchors",
        "memory_anchor_index",
        "memory_seals",
        "procedural_rules",
        "sessions",
        "evidence_spans",
        "search_index_jobs",
        "audit_log",
    ]
    .into_iter()
    .map(|table| {
        db.query(&format!("SELECT * FROM {table} ORDER BY 1, 2"), &[])
            .unwrap()
            .into_iter()
            .map(|row| {
                row.iter()
                    .map(|(name, value)| (name.to_owned(), value.clone()))
                    .collect()
            })
            .collect()
    })
    .collect()
}

fn global_memory(db: &DbConnection, workspace: &str, number: u128, content: &str) -> String {
    let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
    db.insert_memory(
        &id,
        &CreateMemoryInput {
            workspace_id: workspace.to_owned(),
            content: content.to_owned(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            workflow_id: None,
            confidence: 1.0,
            utility: 0.5,
            importance: 0.5,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            provenance_uri: Some("manual://unpublished-global-source".to_owned()),
            tags: Vec::new(),
            valid_from: Some("2000-01-01T00:00:00Z".to_owned()),
            valid_to: None,
        },
    )
    .unwrap();
    id
}

fn publish_store_index(
    root: &Path,
    database: &Path,
    index: &Path,
    workspace: &str,
) -> crate::core::index::TestWorkspaceEmbedderStackGuard {
    let guard = crate::core::index::install_test_hash_workspace_embedder(workspace);
    let report = crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
        workspace_path: root.to_path_buf(),
        database_path: Some(database.to_path_buf()),
        index_dir: Some(index.to_path_buf()),
        dry_run: false,
    })
    .unwrap();
    assert_eq!(
        report.status,
        crate::core::index::IndexRebuildStatus::Success
    );
    guard
}

fn index_file_hashes(root: &Path) -> BTreeMap<PathBuf, blake3::Hash> {
    fn collect(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, blake3::Hash>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                collect(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    blake3::hash(&std::fs::read(path).unwrap()),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    if root.exists() {
        collect(root, root, &mut files);
    }
    files
}

fn complete_lexical(report: &SearchReport) {
    assert_eq!(report.source_mode_applied, SearchSourceMode::LexicalOnly);
    assert!(!report.rerank_runtime_available);
    assert_eq!(report.embed_backend, EmbedBackend::HashFallback);
    assert!(
        report
            .degraded
            .iter()
            .any(|entry| entry.code == "search_live_snapshot_lexical")
    );
    assert!(
        !report
            .degraded
            .iter()
            .any(|entry| entry.code == "search_live_snapshot_unavailable")
    );
    for hit in &report.results {
        assert!(hit.lexical_score.is_some());
        assert!(
            hit.fast_score.is_none() && hit.quality_score.is_none() && hit.rerank_score.is_none()
        );
    }
}

#[test]
fn unpublished_kind_filters_keep_native_rules_and_preserve_read_only_sources() {
    for empty_directory in [false, true] {
        let fixture = Fixture::new();
        let memory_rule = fixture.memory_with_kind(200, BODY, "rule");
        let anti_pattern = fixture.memory_with_kind(204, BODY, "anti-pattern");
        let rule = fixture.rule();
        let deprecated = fixture.rule_with_maturity(205, "deprecated");
        let tombstoned = fixture.rule_with_maturity(206, "validated");
        fixture.db.execute_raw(&format!(
            "UPDATE procedural_rules SET tombstoned_at = '2026-01-01T00:00:00Z' WHERE id = '{tombstoned}'"
        )).unwrap();
        let evidence = fixture.evidence();
        fixture
            .db
            .set_memory_typed_fields_json(&memory_rule, Some(r#"{"condition":"before release"}"#))
            .unwrap();
        if empty_directory {
            std::fs::create_dir(fixture.root.join(".ee/index")).unwrap();
        }
        let before = fixture.state();
        let options = fixture.options();
        let reader = fixture.reader();
        for (kind, field, expected) in [
            (
                None,
                None,
                vec![
                    memory_rule.clone(),
                    anti_pattern.clone(),
                    rule.clone(),
                    deprecated.clone(),
                    evidence,
                ],
            ),
            (
                Some("rule"),
                None,
                vec![memory_rule.clone(), rule.clone(), deprecated.clone()],
            ),
            (Some("anti-pattern"), None, vec![anti_pattern]),
            (
                None,
                Some("condition=before release"),
                vec![memory_rule.clone()],
            ),
            (
                Some("rule"),
                Some("condition=before release"),
                vec![memory_rule.clone()],
            ),
            (
                Some("anti-pattern"),
                Some("condition=before release"),
                Vec::new(),
            ),
        ] {
            let fields: Vec<_> = field
                .map(|field| TypedMemoryFieldFilter::parse(field).unwrap())
                .into_iter()
                .collect();
            let options = &options;
            let reader = &reader;
            let run = with_search_root(|cx| async move {
                let mut audit_ids = SearchAuditIdSource::Ambient;
                run_search_inner_with_performance(
                    &cx,
                    options,
                    SearchMemoryFilters {
                        kind,
                        fields: &fields,
                    },
                    Some(reader),
                    Deterministic::from_seed(0),
                    &mut audit_ids,
                    None,
                    None,
                    true,
                    None,
                    None,
                    None,
                    true,
                )
                .await
            })
            .unwrap();
            complete_lexical(&run.report);
            assert_eq!(
                run.report
                    .results
                    .iter()
                    .map(|hit| hit.doc_id.clone())
                    .collect::<BTreeSet<_>>(),
                expected.iter().cloned().collect::<BTreeSet<_>>(),
                "empty_index={empty_directory}, kind={kind:?}, field={field:?}",
            );
            assert_eq!(
                run.report.status,
                if expected.is_empty() {
                    SearchStatus::NoResults
                } else {
                    SearchStatus::Success
                }
            );
            assert_eq!(fixture.state(), before);
        }
        if empty_directory {
            assert!(
                std::fs::read_dir(options.resolve_index_dir())
                    .unwrap()
                    .next()
                    .is_none()
            );
        } else {
            assert!(!options.resolve_index_dir().exists());
        }
    }
}

#[test]
fn absent_and_empty_indexes_search_native_memory_rule_and_cass_sources_without_aliases() {
    const GLOBAL_CHILD: &str = "EE_TEST_UNPUBLISHED_GLOBAL_STORE";
    if std::env::var_os(GLOBAL_CHILD).is_some() {
        assert_global_sources_without_publication();
        return;
    }
    for empty_directory in [false, true] {
        let fixture = Fixture::new();
        let memory = fixture.memory(200, BODY);
        let rule = fixture.rule();
        let evidence = fixture.evidence();
        if empty_directory {
            std::fs::create_dir(fixture.root.join(".ee/index")).unwrap();
        }
        let before = fixture.state();
        let report =
            run_search_with_read_connection(&fixture.options(), &fixture.reader()).unwrap();
        assert_eq!(report.status, SearchStatus::Success);
        complete_lexical(&report);
        let ids: BTreeSet<_> = report
            .results
            .iter()
            .map(|hit| hit.doc_id.clone())
            .collect();
        assert_eq!(ids, BTreeSet::from([memory, rule, evidence]));
        assert_eq!(fixture.db.count_table_rows("memories").unwrap(), 1);
        assert_eq!(fixture.state(), before);
        if empty_directory {
            assert!(
                std::fs::read_dir(fixture.root.join(".ee/index"))
                    .unwrap()
                    .next()
                    .is_none()
            );
        } else {
            assert!(!fixture.root.join(".ee/index").exists());
        }
    }
    // Isolate XDG resolution in a subprocess; never mutate this test
    // process's environment while the other retrieval tests run in parallel.
    let global_root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("core::search::unpublished_tests::absent_and_empty_indexes_search_native_memory_rule_and_cass_sources_without_aliases")
        .arg("--nocapture")
        .env(GLOBAL_CHILD, "1")
        .env("XDG_DATA_HOME", global_root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "global source child failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

fn assert_global_sources_without_publication() {
    let fixture = Fixture::new();
    let local = fixture.memory(290, BODY);
    std::fs::write(
        fixture.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = true\n",
    )
    .unwrap();
    let paths = crate::core::global_store::default_global_store_paths_from_env().unwrap();
    let (global, workspace) =
        crate::core::global_store::open_or_create_global_store(&paths).unwrap();
    let insert = |number, content: &str| global_memory(&global, &workspace, number, content);
    let global_body = "The unpublishedquasar deployment needs signed release attestations.";
    let visible = insert(291, global_body);
    let sealed = insert(292, crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT);
    let tombstoned = insert(293, global_body);
    global
        .insert_memory_seal(
            &sealed,
            &format!("blake3:{}", "a".repeat(64)),
            "2020-01-01T00:00:00Z",
        )
        .unwrap();
    global.tombstone_memory(&tombstoned).unwrap();
    let before = fixture.state();
    let global_before = source_state(&global);
    let handoff = run_pack_search(&fixture.options()).unwrap();
    complete_lexical(&handoff.report);
    assert!(handoff.can_reuse_for_pack());
    assert_eq!(
        handoff
            .report
            .results
            .iter()
            .map(|hit| hit.doc_id.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([local.as_str(), visible.as_str()])
    );
    let response = crate::core::context::run_context_pack(&fixture.pack_options()).unwrap();
    assert!(response.success);
    assert!(
        response
            .data
            .pack
            .items
            .iter()
            .any(|item| item.memory_id.to_string() == visible),
        "an unpublished global memory must remain available to read-only packs"
    );
    assert!(response.data.pack.items.iter().all(|item| {
        item.memory_id.to_string() != sealed && item.memory_id.to_string() != tombstoned
    }));
    assert_eq!(fixture.state(), before);
    assert_eq!(source_state(&global), global_before);
    assert!(!fixture.root.join(".ee/index").exists());
    assert!(!paths.index_dir.exists());
    std::fs::write(
        fixture.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = false\n",
    )
    .unwrap();
    let excluded = run_pack_search(&fixture.options()).unwrap();
    assert_eq!(excluded.report.results.len(), 1);
    assert_eq!(excluded.report.results[0].doc_id, local);
    assert_eq!(source_state(&global), global_before);
}

#[test]
fn healthy_workspace_searches_missing_and_stale_global_indexes_without_writes() {
    const GLOBAL_CHILD: &str = "EE_TEST_UNPUBLISHED_GLOBAL_HEALTHY_WORKSPACE";
    if std::env::var_os(GLOBAL_CHILD).is_some() {
        assert_global_sources_with_healthy_workspace();
        return;
    }
    let global_root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("core::search::unpublished_tests::healthy_workspace_searches_missing_and_stale_global_indexes_without_writes")
        .arg("--nocapture")
        .env(GLOBAL_CHILD, "1")
        .env("XDG_DATA_HOME", global_root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "global source child failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

fn assert_global_sources_with_healthy_workspace() {
    let fixture = Fixture::new();
    let local = fixture.memory(300, BODY);
    let local_index = fixture.root.join(".ee/index");
    let _local_stack = publish_store_index(
        &fixture.root,
        &fixture.root.join(".ee/ee.db"),
        &local_index,
        &fixture.workspace,
    );
    std::fs::write(
        fixture.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = true\n",
    )
    .unwrap();
    let paths = crate::core::global_store::default_global_store_paths_from_env().unwrap();
    let (global, workspace) =
        crate::core::global_store::open_or_create_global_store(&paths).unwrap();
    let insert = |number, content: &str| global_memory(&global, &workspace, number, content);
    let visible = insert(
        301,
        "The unpublishedquasar deployment needs signed attestations.",
    );
    let excluded = insert(
        302,
        &format!("{BODY} {} forbidden", "ordinary context ".repeat(512)),
    );
    let sealed = insert(303, BODY);
    let tombstoned = insert(304, BODY);
    let retired = insert(305, BODY);
    global
        .insert_memory_seal(
            &sealed,
            &format!("blake3:{}", "a".repeat(64)),
            "2020-01-01T00:00:00Z",
        )
        .unwrap();
    global.tombstone_memory(&tombstoned).unwrap();
    global
        .mark_memory_superseded(&retired, "2020-01-01T00:00:00Z")
        .unwrap();
    let mut expected_global = BTreeSet::from([visible.clone(), excluded.clone()]);
    for phase in ["missing", "stale", "deferred"] {
        let stale = phase != "missing";
        let _global_stack = (phase == "stale").then(|| {
            publish_store_index(
                &paths.root,
                &paths.database_path,
                &paths.index_dir,
                &workspace,
            )
        });
        if phase == "stale" {
            let fresh = insert(
                306,
                "The unpublishedquasar rollout needs the new approval receipt.",
            );
            global
                .insert_search_index_job(
                    "sidx_00000000000000000000000306",
                    &crate::db::CreateSearchIndexJobInput {
                        workspace_id: workspace.clone(),
                        job_type: crate::db::SearchIndexJobType::SingleDocument,
                        document_source: Some("memory".to_owned()),
                        document_id: Some(fresh.clone()),
                        documents_total: 1,
                    },
                )
                .unwrap();
            expected_global.insert(fresh);
        }
        if phase == "deferred" {
            global
                .with_transaction(|| {
                    for number in 10_000..12_049 {
                        insert(number, "An unrelated user convention about editor colors.");
                    }
                    Ok(())
                })
                .unwrap();
            assert!(matches!(
                crate::core::index::get_index_status_for_lexical_search(
                    &IndexStatusOptions {
                        workspace_path: paths.root.clone(),
                        database_path: Some(paths.database_path.clone()),
                        index_dir: Some(paths.index_dir.clone()),
                    },
                    None,
                    false,
                ),
                Err(IndexStatusError::SourceInspectionDeferred { .. })
            ));
        }
        let local_before = fixture.state();
        let global_before = source_state(&global);
        let local_index_before = index_file_hashes(&local_index);
        let global_index_before = index_file_hashes(&paths.index_dir);
        let options = fixture.options();
        let handoff = run_pack_search(&options).unwrap();
        assert!(handoff.can_reuse_for_pack());
        assert!(
            handoff
                .report
                .index_freshness
                .as_ref()
                .is_none_or(|status| !status.stale)
        );
        assert!(
            handoff
                .report
                .degraded
                .iter()
                .any(|entry| entry.code == "global_live_snapshot_lexical"),
            "phase={phase} degraded={:?}",
            handoff.report.degraded
        );
        assert!(!handoff.report.degraded.iter().any(|entry| matches!(
            entry.code.as_str(),
            "global_index_unavailable" | "search_live_snapshot_lexical"
        )));
        let mut expected = expected_global.clone();
        expected.insert(local.clone());
        assert_eq!(
            handoff
                .report
                .results
                .iter()
                .map(|hit| hit.doc_id.clone())
                .collect::<BTreeSet<_>>(),
            expected
        );

        let mut global_only = options.clone();
        global_only.memory_scope = MemoryScope::Global;
        let global_report = run_pack_search(&global_only).unwrap().report;
        assert_eq!(
            global_report
                .results
                .iter()
                .map(|hit| hit.doc_id.clone())
                .collect::<BTreeSet<_>>(),
            expected_global
        );
        let mut negative = options.clone();
        negative.query = format!("{WORD} -forbidden");
        expected.remove(&excluded);
        let negative_report =
            run_search_with_read_connection(&negative, &fixture.reader()).unwrap();
        assert_eq!(
            negative_report
                .results
                .iter()
                .map(|hit| hit.doc_id.clone())
                .collect::<BTreeSet<_>>(),
            expected
        );

        let mut pack_options = fixture.pack_options();
        pack_options.query = negative.query;
        let response = crate::core::context::run_context_pack(&pack_options).unwrap();
        assert!(response.success);
        assert!(
            response
                .data
                .pack
                .items
                .iter()
                .any(|item| item.memory_id.to_string() == visible)
        );
        assert!(
            response
                .data
                .pack
                .items
                .iter()
                .all(|item| expected.contains(&item.memory_id.to_string()))
        );

        let mut historical = options.clone();
        historical.as_of = Some(Utc::now());
        let history = run_pack_search(&historical).unwrap().report;
        assert!(
            !history
                .degraded
                .iter()
                .any(|entry| entry.code == "global_live_snapshot_lexical")
        );
        assert!(
            history
                .degraded
                .iter()
                .any(|entry| entry.code == "global_index_unavailable")
        );
        assert_eq!(
            history
                .results
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            vec![local.as_str()]
        );

        std::fs::write(
            fixture.root.join(".ee/config.toml"),
            "[memory]\ninclude_global = false\n",
        )
        .unwrap();
        let disabled = run_pack_search(&global_only).unwrap().report;
        assert!(disabled.results.is_empty());
        assert!(
            disabled
                .degraded
                .iter()
                .any(|entry| entry.code == "global_lane_disabled")
        );
        std::fs::write(
            fixture.root.join(".ee/config.toml"),
            "[memory]\ninclude_global = true\n",
        )
        .unwrap();
        assert_eq!(fixture.state(), local_before);
        assert_eq!(source_state(&global), global_before);
        assert_eq!(index_file_hashes(&local_index), local_index_before);
        assert_eq!(index_file_hashes(&paths.index_dir), global_index_before);
        assert_eq!(paths.index_dir.exists(), stale);
    }

    // Incompatibility and corruption are not ordinary generation lag. Both
    // published and source-only workspace reads must retain the refusal.
    let metadata_path = paths.index_dir.join("meta.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
    metadata["corpusRevision"] = serde_json::json!("incompatible-global-corpus");
    let unpublished = Fixture::new();
    let unpublished_local = unpublished.memory(307, BODY);
    std::fs::write(
        unpublished.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = true\n",
    )
    .unwrap();
    for bytes in [
        serde_json::to_vec(&metadata).unwrap(),
        b"corrupt-global-metadata".to_vec(),
    ] {
        std::fs::write(&metadata_path, &bytes).unwrap();
        for (fixture, expected_local) in [(&fixture, &local), (&unpublished, &unpublished_local)] {
            let report = run_pack_search(&fixture.options()).unwrap().report;
            assert!(
                report
                    .degraded
                    .iter()
                    .any(|entry| entry.code == "global_index_unavailable")
            );
            assert!(
                !report
                    .degraded
                    .iter()
                    .any(|entry| entry.code == "global_live_snapshot_lexical")
            );
            assert_eq!(
                report
                    .results
                    .iter()
                    .map(|hit| &hit.doc_id)
                    .collect::<Vec<_>>(),
                vec![expected_local]
            );
        }
        assert_eq!(std::fs::read(&metadata_path).unwrap(), bytes);
    }
    #[cfg(unix)]
    {
        std::fs::rename(&paths.index_dir, paths.root.join("rejected-index")).unwrap();
        std::os::unix::fs::symlink(paths.root.join("nonexistent-target"), &paths.index_dir)
            .unwrap();
        let report = run_pack_search(&fixture.options()).unwrap().report;
        assert!(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "global_index_unavailable")
        );
        assert!(
            !report
                .degraded
                .iter()
                .any(|entry| entry.code == "global_live_snapshot_lexical")
        );
        assert_eq!(
            report
                .results
                .iter()
                .map(|hit| &hit.doc_id)
                .collect::<Vec<_>>(),
            vec![&local]
        );
        assert!(
            std::fs::symlink_metadata(&paths.index_dir)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[cfg(unix)]
#[test]
fn global_search_keeps_bodies_feedback_and_scores_in_the_same_retained_generation() {
    let fixture = Fixture::new();
    let paths =
        super::super::global_store::GlobalStorePaths::from_root(&fixture.root.join("global"));
    let (global, workspace) =
        super::super::global_store::open_or_create_global_store(&paths).unwrap();
    let memory = global_memory(&global, &workspace, 401, BODY);
    let reference = Utc::now();
    global
        .insert_feedback_event_for_recovery(&StoredFeedbackEvent {
            id: "fb_00000000000000000000000401".to_owned(),
            workspace_id: workspace.clone(),
            target_type: "memory".to_owned(),
            target_id: memory.clone(),
            signal: "harmful".to_owned(),
            weight: 1.0,
            source_type: "human_explicit".to_owned(),
            source_id: None,
            reason: None,
            evidence_json: None,
            session_id: None,
            applied_at: None,
            created_at: (reference - chrono::Duration::seconds(1)).to_rfc3339(),
        })
        .unwrap();
    let _old_stack = publish_store_index(
        &paths.root,
        &paths.database_path,
        &paths.index_dir,
        &workspace,
    );
    let options = fixture.options();
    let old_query = capture_global_search(&paths, &options, reference);
    let new_query = capture_global_search(&paths, &options, reference);
    let old_generation = old_query.generation.unwrap();

    // Force the exact production race: body/feedback admission has completed,
    // then another writer changes the same ID and publishes a new corpus.
    rewrite_global_memory(
        &global,
        &workspace,
        &memory,
        "Racingnebula uses different release rules.",
    );
    global
        .execute_raw("UPDATE feedback_events SET signal = 'helpful'")
        .unwrap();
    global_memory(
        &global,
        &workspace,
        402,
        "Racingnebula has a newly added convention.",
    );
    let _new_stack = publish_store_index(
        &paths.root,
        &paths.database_path,
        &paths.index_dir,
        &workspace,
    );
    assert!(
        global
            .get_workspace_generation(&workspace)
            .unwrap()
            .unwrap()
            > old_generation
    );
    assert_eq!(
        old_query
            .source
            .connection()
            .get_workspace_generation(&workspace)
            .unwrap(),
        Some(old_generation)
    );
    let before = index_file_hashes(&paths.root);

    let old = retrieve_captured_global_search(&paths, &options, old_query);
    assert_eq!(old.hits.len(), 1);
    assert_eq!(old.hits[0].doc_id, memory);
    assert!(old.hits[0].lexical_score.is_some_and(|score| score > 0.0));
    assert_eq!(old.memories[&memory].content, BODY);
    assert_eq!(old.memories[&memory].confidence, 1.0);
    assert_eq!(
        old.hits[0].metadata.as_ref().unwrap()[SEARCH_ANALYSIS_CONTENT_KEY],
        BODY
    );
    assert_eq!(
        old.feedback
            .as_ref()
            .unwrap()
            .harmful_count(&old.memories[&memory], reference),
        Some(1)
    );
    assert!(!old.degraded.iter().any(|entry| matches!(
        entry.code.as_str(),
        "global_index_unavailable" | "global_live_snapshot_lexical"
    )));

    let mut changed_query = options.clone();
    changed_query.query = "racingnebula".to_owned();
    let changed = retrieve_captured_global_search(&paths, &changed_query, new_query);
    assert!(
        changed.hits.is_empty(),
        "new index terms must not relabel old source bodies"
    );
    assert!(changed.memories.is_empty());
    assert!(
        !changed
            .degraded
            .iter()
            .any(|entry| entry.code == "global_index_unavailable")
    );
    let fresh_reference = Utc::now();
    let fresh = retrieve_captured_global_search(
        &paths,
        &changed_query,
        capture_global_search(&paths, &changed_query, fresh_reference),
    );
    assert_eq!(fresh.hits.len(), 2);
    assert!(fresh.memories[&memory].content.contains("Racingnebula"));
    assert_eq!(
        fresh
            .feedback
            .as_ref()
            .unwrap()
            .harmful_count(&fresh.memories[&memory], fresh_reference),
        Some(0)
    );
    assert_eq!(index_file_hashes(&paths.root), before);
}

#[cfg(unix)]
#[test]
fn ready_global_index_refills_past_retired_rows_from_the_admitted_snapshot() {
    let fixture = Fixture::new();
    let paths =
        super::super::global_store::GlobalStorePaths::from_root(&fixture.root.join("global"));
    let (global, workspace) =
        super::super::global_store::open_or_create_global_store(&paths).unwrap();
    // Historical rows remain indexed. Their short exact-match bodies outrank
    // the one current row and fill the first 32 native candidates completely.
    for number in 500..533 {
        let retired = global_memory(&global, &workspace, number, WORD);
        global
            .mark_memory_superseded(&retired, "2020-01-01T00:00:00Z")
            .unwrap();
    }
    let visible = global_memory(
        &global,
        &workspace,
        550,
        &format!("{WORD} {}", "unrelated background context ".repeat(512)),
    );
    let _stack = publish_store_index(
        &paths.root,
        &paths.database_path,
        &paths.index_dir,
        &workspace,
    );
    let options = fixture.options();
    let captured = capture_global_search(&paths, &options, Utc::now());
    assert_eq!(captured.memories.len(), 1);
    let before = index_file_hashes(&paths.root);
    let result = retrieve_captured_global_search(&paths, &options, captured);
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].doc_id, visible);
    assert!(!result.degraded.iter().any(|entry| matches!(
        entry.code.as_str(),
        "global_index_unavailable" | "global_live_snapshot_lexical"
    )));
    assert_eq!(index_file_hashes(&paths.root), before);
}

#[cfg(unix)]
#[test]
fn global_search_uses_captured_corpus_when_publication_has_no_matching_retained_generation() {
    let fixture = Fixture::new();
    let paths =
        super::super::global_store::GlobalStorePaths::from_root(&fixture.root.join("global"));
    let (global, workspace) =
        super::super::global_store::open_or_create_global_store(&paths).unwrap();
    let original = global_memory(&global, &workspace, 411, BODY);
    let _old_stack = publish_store_index(
        &paths.root,
        &paths.database_path,
        &paths.index_dir,
        &workspace,
    );
    let fresh = global_memory(&global, &workspace, 412, BODY);
    let options = fixture.options();
    let captured = capture_global_search(&paths, &options, Utc::now());
    let captured_generation = captured.generation.unwrap();
    rewrite_global_memory(
        &global,
        &workspace,
        &original,
        "Racingnebula changed after admission.",
    );
    let _new_stack = publish_store_index(
        &paths.root,
        &paths.database_path,
        &paths.index_dir,
        &workspace,
    );
    assert!(
        global
            .get_workspace_generation(&workspace)
            .unwrap()
            .unwrap()
            > captured_generation
    );
    let before = index_file_hashes(&paths.root);
    let result = retrieve_captured_global_search(&paths, &options, captured);
    assert_eq!(
        result
            .hits
            .iter()
            .map(|hit| hit.doc_id.clone())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([original, fresh])
    );
    assert!(
        result
            .memories
            .values()
            .all(|memory| memory.content == BODY)
    );
    assert!(
        result
            .degraded
            .iter()
            .any(|entry| entry.code == "global_live_snapshot_lexical")
    );
    assert!(
        !result
            .degraded
            .iter()
            .any(|entry| entry.code == "global_index_unavailable")
    );
    assert_eq!(index_file_hashes(&paths.root), before);
}

#[cfg(unix)]
#[test]
fn global_snapshot_overtaken_by_first_publication_keeps_fallback_policy_and_integrity() {
    let fixture = Fixture::new();
    let paths =
        super::super::global_store::GlobalStorePaths::from_root(&fixture.root.join("global"));
    let (global, workspace) =
        super::super::global_store::open_or_create_global_store(&paths).unwrap();
    let memory = global_memory(&global, &workspace, 421, BODY);
    let reference = Utc::now();
    let options = fixture.options();
    let mut variants = vec![("current", options.clone()), ("new terms", options.clone())];
    variants[1].1.query = "racingnebula".to_owned();
    let mut historical = options.clone();
    historical.as_of = Some(reference);
    variants.push(("historical", historical));
    let mut tombstones = options.clone();
    tombstones.include_tombstoned = true;
    variants.push(("tombstones", tombstones));
    for mode in [SearchSourceMode::Hybrid, SearchSourceMode::SemanticOnly] {
        let mut strict = options.clone();
        strict.source_mode = mode;
        strict.strict_source_mode = true;
        variants.push(("strict semantic", strict));
    }
    let captures = variants
        .into_iter()
        .map(|(label, options)| {
            let captured = capture_global_search(&paths, &options, reference);
            (label, options, captured)
        })
        .collect::<Vec<_>>();
    let corrupt_capture = capture_global_search(&paths, &options, reference);
    rewrite_global_memory(
        &global,
        &workspace,
        &memory,
        "Racingnebula replaced the unpublished advice.",
    );
    let _stack = publish_store_index(
        &paths.root,
        &paths.database_path,
        &paths.index_dir,
        &workspace,
    );
    let before = index_file_hashes(&paths.root);
    for (label, options, captured) in captures {
        let result = retrieve_captured_global_search(&paths, &options, captured);
        // Only current lexical reads can recover the overtaken source from
        // its pinned bodies. Strict vector reads require the matching index.
        if matches!(label, "current" | "new terms") {
            assert!(
                result
                    .degraded
                    .iter()
                    .any(|entry| entry.code == "global_live_snapshot_lexical"),
                "{label}"
            );
            assert!(
                !result
                    .degraded
                    .iter()
                    .any(|entry| entry.code == "global_index_unavailable"),
                "{label}"
            );
            if label != "new terms" {
                assert_eq!(result.hits.len(), 1);
                assert_eq!(result.hits[0].doc_id, memory);
                assert_eq!(result.memories[&memory].content, BODY);
            } else {
                assert!(result.hits.is_empty());
            }
        } else {
            assert!(result.hits.is_empty(), "{label}");
            assert!(
                result
                    .degraded
                    .iter()
                    .any(|entry| entry.code == "global_index_unavailable"),
                "{label}"
            );
            assert!(
                !result
                    .degraded
                    .iter()
                    .any(|entry| entry.code == "global_live_snapshot_lexical"),
                "{label}"
            );
        }
    }
    assert_eq!(index_file_hashes(&paths.root), before);

    // A newer watermark is no excuse to ignore damaged persisted tiers.
    std::fs::write(
        paths.index_dir.join("vector.fast.idx"),
        b"broken global vectors",
    )
    .unwrap();
    let corrupt_before = index_file_hashes(&paths.root);
    let rejected = retrieve_captured_global_search(&paths, &options, corrupt_capture);
    assert!(rejected.hits.is_empty());
    assert!(
        rejected
            .degraded
            .iter()
            .any(|entry| entry.code == "global_index_unavailable")
    );
    assert!(
        !rejected
            .degraded
            .iter()
            .any(|entry| entry.code == "global_live_snapshot_lexical")
    );
    assert_eq!(index_file_hashes(&paths.root), corrupt_before);
}

#[cfg(unix)]
fn capture_global_search(
    paths: &super::super::global_store::GlobalStorePaths,
    options: &SearchOptions,
    reference: DateTime<Utc>,
) -> GlobalStoreSearchSnapshot {
    read_global_search_memories(paths, options, SearchMemoryFilters::default(), reference)
        .unwrap()
        .unwrap()
}

#[cfg(unix)]
fn rewrite_global_memory(db: &DbConnection, workspace: &str, memory: &str, content: &str) {
    assert!(
        db.apply_memory_curation_update(
            memory,
            &crate::db::ApplyMemoryCurationInput {
                workspace_id: workspace.to_owned(),
                content: content.to_owned(),
                confidence: 0.4,
                trust_class: "human_explicit".to_owned(),
            },
        )
        .unwrap()
    );
}

#[cfg(unix)]
struct CapturedGlobalSearchResult {
    hits: Vec<SearchHit>,
    degraded: Vec<SearchDegradation>,
    memories: BTreeMap<String, StoredMemory>,
    feedback: Option<GlobalMemoryFeedbackSnapshot>,
}

#[cfg(unix)]
fn retrieve_captured_global_search(
    paths: &super::super::global_store::GlobalStorePaths,
    options: &SearchOptions,
    snapshot: GlobalStoreSearchSnapshot,
) -> CapturedGlobalSearchResult {
    crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        let mut degraded = Vec::new();
        let mut memories = BTreeMap::new();
        let mut feedback = None;
        let producer: Arc<dyn Embedder> = Arc::new(FirstPublicationEmbedder::new(false));
        let hits = global_store_snapshot_frankensearch_hits(
            &cx,
            options,
            paths,
            snapshot,
            32,
            &mut degraded,
            Some(&mut memories),
            &mut feedback,
            &mut SearchPerformanceTrace::default(),
            GlobalSearchExecution {
                mode: options.execution_source_mode(),
                producer: Some(&producer),
                fusion_weights: SearchFusionWeights::default(),
            },
            false,
        )
        .await
        .hits;
        CapturedGlobalSearchResult {
            hits,
            degraded,
            memories,
            feedback,
        }
    })
    .unwrap()
}

#[test]
fn queued_writes_are_searchable_before_publication_without_consuming_their_jobs() {
    let fixture = Fixture::new();
    let memory = fixture.memory(210, BODY);
    fixture
        .db
        .insert_search_index_job(
            "sidx_00000000000000000000000210",
            &crate::db::CreateSearchIndexJobInput {
                workspace_id: fixture.workspace.clone(),
                job_type: crate::db::SearchIndexJobType::SingleDocument,
                document_source: Some("memory".to_owned()),
                document_id: Some(memory.clone()),
                documents_total: 1,
            },
        )
        .unwrap();
    let before = fixture.state();
    let mut options = fixture.options();
    options.source_mode = SearchSourceMode::Hybrid;
    let report = run_search_with_read_connection(&options, &fixture.reader()).unwrap();
    complete_lexical(&report);
    assert!(report.source_mode_fallback);
    assert_eq!(report.results[0].doc_id, memory);
    assert_eq!(fixture.state(), before);
    assert!(!fixture.root.join(".ee/index").exists());
}

// Deterministic vectors exercise semantic dispatch and publication without a
// model download. Cancellation belongs to the publishing child context, so a
// later ordinary search can prove the durable job remains recoverable.
#[cfg(unix)]
struct FirstPublicationEmbedder {
    hash: HashEmbedder,
    calls: AtomicUsize,
    cancel_next: std::sync::atomic::AtomicBool,
}

#[cfg(unix)]
impl FirstPublicationEmbedder {
    fn new(cancel_next: bool) -> Self {
        Self {
            hash: HashEmbedder::default_256(),
            calls: AtomicUsize::new(0),
            cancel_next: std::sync::atomic::AtomicBool::new(cancel_next),
        }
    }
}

#[cfg(unix)]
impl Embedder for FirstPublicationEmbedder {
    fn embed<'a>(
        &'a self,
        cx: &'a asupersync::Cx,
        text: &'a str,
    ) -> frankensearch::SearchFuture<'a, Vec<f32>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.cancel_next.swap(false, Ordering::Relaxed) {
            cx.cancel_with(
                asupersync::CancelKind::User,
                Some("first-publication fixture cancellation"),
            );
        }
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

    fn is_semantic(&self) -> bool {
        true
    }

    fn category(&self) -> frankensearch::core::traits::ModelCategory {
        self.hash.category()
    }
}

#[cfg(unix)]
#[test]
fn embedding_search_publishes_first_generation_from_deferred_remember() {
    with_test_search_timeouts(Duration::from_secs(300), || {
        for (empty_directory, documents, mode) in [
            (false, 1, SearchSourceMode::Hybrid),
            (true, 1, SearchSourceMode::SemanticOnly),
            (false, 64, SearchSourceMode::SemanticOnly),
        ] {
            let fixture = Fixture::new();
            let index_dir = fixture.root.join(".ee/index");
            if empty_directory {
                std::fs::create_dir(&index_dir).unwrap();
            }
            for number in 1..documents {
                fixture.memory(
                    u128::from(number),
                    &format!("Earlier deployment note {number}."),
                );
            }
            let remembered = fixture.remember_deferred(BODY);
            let embedder = Arc::new(FirstPublicationEmbedder::new(false));
            let _selection = crate::core::index::install_test_workspace_embedder(
                &fixture.workspace,
                EmbedderStack::from_parts(embedder.clone(), None),
            );
            let mut options = fixture.options();
            options.query = BODY.to_owned();
            options.source_mode = mode;
            options.strict_source_mode = true;

            let run = run_search_with_performance(&options).unwrap();
            assert_eq!(
                run.report.results[0].doc_id,
                remembered.memory_id.to_string()
            );
            assert!(run.report.results[0].fast_score.is_some());
            assert_eq!(run.report.source_mode_applied, mode);
            assert!(!run.report.source_mode_fallback);
            assert!(run.report.index_freshness.is_none());
            assert!(!run.report.degraded.iter().any(|entry| matches!(
                entry.code.as_str(),
                "index_missing" | "search_index_stale" | "search_live_snapshot_lexical"
            )));
            assert!(embedder.calls.load(Ordering::Relaxed) > 0);
            let status = crate::core::index::get_index_status(&IndexStatusOptions {
                workspace_path: fixture.root.clone(),
                database_path: Some(fixture.root.join(".ee/ee.db")),
                index_dir: Some(index_dir),
            })
            .unwrap();
            assert_eq!(status.health, IndexHealth::Ready);
            assert_eq!(status.index_generation, status.db_generation);
            assert_eq!(status.index_document_count, Some(documents));
            let jobs = fixture
                .db
                .list_search_index_jobs(&fixture.workspace, None)
                .unwrap();
            assert_eq!(jobs.len(), 1, "search must only consume the remembered job");
            assert_eq!(Some(&jobs[0].id), remembered.index_job_id.as_ref());
            assert_eq!(jobs[0].status, "completed");
        }
    });
}

/// Why the pre-read repair left a delta unpublished, as far as a test can see.
///
/// The repair can stop at several points, and it reports none of them to the
/// caller, only to tracing, which is not captured here. So a red could not
/// name its cause (bd-q1e4y: C75z5 at 3ed75b4a1 showed one embed call since
/// the remember, the index one generation behind, and nothing else). That
/// one call fits the query alone, so the delta may never have been embedded.
/// This renders the job rows and re-evaluates each stage on the same store.
/// Stages 1 to 3 only read, so the order they run in changes nothing. The
/// processor can publish, so it runs last:
/// 1. the source upper bound against the full-rebuild ceiling, where `Some(false)`
///    selects the bounded path;
/// 2. the bounded generation gap, where `Ok(None)` skips the repair;
/// 3. the streamed publisher's metadata validation against the workspace's
///    selected producer. Gate 2 performs only structural admission; the
///    publisher also verifies native vector producer identities before reuse;
/// 4. the bounded coalesced processor, with the same one-job limit, since both
///    callers queue exactly one delta. A job reported skipped as "deferred"
///    means the streamed delta was declined. The census beside it shows the
///    index directory against that path's generation limits: 512 entries,
///    64 MiB, depth 16, regular files only, and no symlinked component.
///
/// Call it only from a failing assertion's message. The processor can
/// publish, and by then the observation it would change has been made.
#[cfg(unix)]
fn delta_publication_probe(
    fixture: &Fixture,
    options: &SearchOptions,
    producer: &dyn Embedder,
) -> String {
    let jobs = fixture
        .db
        .list_search_index_jobs(&fixture.workspace, None)
        .map(|jobs| {
            jobs.iter()
                .map(|job| format!("{}={}:{:?}", job.id, job.status, job.error_message))
                .collect::<Vec<_>>()
        });
    let index_dir = options.resolve_index_dir();
    let census = index_dir_census(&index_dir);
    let symlink_free = crate::core::index::ensure_index_path_has_no_symlinks(
        &index_dir,
        "probe the index directory",
    );
    let delta_gate = match fixture.db.get_workspace_generation(&fixture.workspace) {
        Ok(generation) => crate::core::index::delta_metadata_gate_for_test(
            &index_dir,
            generation.unwrap_or(0),
            producer,
        ),
        Err(error) => format!("database generation unreadable: {error}"),
    };
    let status_options = IndexStatusOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: options.database_path.clone(),
        index_dir: Some(index_dir.clone()),
    };
    let upper_bound_fits = crate::core::index::index_source_upper_bound_fits(
        &status_options,
        u32::try_from(super::SEARCH_INDEX_AUTO_RECONCILE_MAX_DOCUMENTS).unwrap_or(u32::MAX),
    );
    let processing = crate::core::index::IndexProcessingOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: options.database_path.clone(),
        index_dir: Some(index_dir),
        dry_run: false,
        job_limit: Some(1),
    };
    let staged = crate::core::run_cli_with_cx(Duration::from_secs(300), |cx| async move {
        let gap = crate::core::index::bounded_index_delta_generation_gap(
            &cx,
            &status_options,
            super::SEARCH_INDEX_LARGE_GAP_THRESHOLD,
        );
        let processed = crate::core::index::process_index_jobs_coalesced_with_cx_bounded(
            &cx,
            &processing,
            u32::try_from(super::SEARCH_INDEX_AUTO_RECONCILE_MAX_DOCUMENTS).unwrap_or(u32::MAX),
        )
        .await;
        (gap, processed)
    });
    // Only building the runtime fails here, and that must not discard the
    // stages already read outside it. A budget expiry is not this arm: it
    // comes back inside `gap` or `processed` as their own error.
    let (gap, processed) = match staged {
        Ok((gap, processed)) => (
            format!("{gap:?}"),
            processed.map_err(|error| format!("error: {error:?}")),
        ),
        Err(error) => (
            "not returned".to_owned(),
            Err(format!("runtime error: {error:?}")),
        ),
    };
    let processed = match processed {
        Ok(report) => format!(
            "status {} pending {} completed {} failed {} jobs {:?}",
            report.status.as_str(),
            report.pending_jobs,
            report.completed_jobs,
            report.failed_jobs,
            report
                .jobs
                .iter()
                .map(|job| {
                    (
                        job.outcome.as_str(),
                        job.processing_mode.as_str(),
                        job.fallback_to_full.as_deref(),
                        job.error.as_deref(),
                    )
                })
                .collect::<Vec<_>>()
        ),
        Err(error) => error,
    };
    format!(
        "jobs before probe {jobs:?}; 1. upper bound fits {upper_bound_fits:?}; \
         2. bounded gap {gap}; 3. delta metadata gate {delta_gate}; \
         4. processor {processed}; \
         index dir {census}, symlink-free {symlink_free:?}"
    )
}

/// The index directory as the streamed-delta path measures it: entries,
/// regular-file bytes and depth below the root, plus anything that is neither
/// a regular file nor a directory, which that path refuses outright.
#[cfg(unix)]
fn index_dir_census(index_dir: &std::path::Path) -> String {
    let mut pending = vec![(index_dir.to_path_buf(), 0_usize)];
    let (mut entries, mut bytes, mut depth_seen) = (0_usize, 0_u64, 0_usize);
    let mut irregular = Vec::new();
    while let Some((directory, depth)) = pending.pop() {
        let listing = match std::fs::read_dir(&directory) {
            Ok(listing) => listing,
            Err(error) => return format!("unreadable at {}: {error}", directory.display()),
        };
        for entry in listing.flatten() {
            entries += 1;
            match std::fs::symlink_metadata(entry.path()) {
                Ok(metadata) if metadata.is_dir() => {
                    depth_seen = depth_seen.max(depth + 1);
                    pending.push((entry.path(), depth + 1));
                }
                Ok(metadata) if metadata.is_file() => bytes += metadata.len(),
                _ => irregular.push(entry.file_name()),
            }
        }
    }
    format!("{entries} entries, {bytes} bytes, depth {depth_seen}, irregular {irregular:?}")
}

#[cfg(unix)]
#[test]
fn strict_semantic_search_publishes_a_small_delta_without_reembedding_a_larger_corpus() {
    // A daemon may already have used its process default for another
    // workspace. Isolate the OnceLock so this regression is deterministic.
    const CHILD: &str = "EE_TEST_DELTA_WITH_FOREIGN_DEFAULT";
    const SENTINEL: &str = "workspace delta published with a different process model";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "core::search::unpublished_tests::strict_semantic_search_publishes_a_small_delta_without_reembedding_a_larger_corpus",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("EE_EMBED_BACKEND", "local")
            .env("EE_EMBED_DOWNLOAD", "off")
            .output()
            .expect("isolated delta publication test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains(SENTINEL),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let foreign = FirstPublicationEmbedder {
        hash: HashEmbedder::new(128, frankensearch::HashAlgorithm::JLProjection { seed: 71 }),
        ..FirstPublicationEmbedder::new(false)
    };
    crate::core::index::install_test_process_embedder(EmbedderStack::from_parts(
        Arc::new(foreign),
        None,
    ));
    with_test_search_timeouts(Duration::from_secs(300), || {
        const RETAINED_BODY: &str =
            "The archivedcobalt upgrade requires an immutable rollback plan.";
        let fixture = Fixture::new();
        let retained_memory = fixture.memory(1, RETAINED_BODY);
        for number in 2..=128_u128 {
            fixture.memory(number, &format!("Earlier deployment note {number}."));
        }
        let embedder = Arc::new(FirstPublicationEmbedder::new(false));
        let _selection = crate::core::index::install_test_workspace_embedder(
            &fixture.workspace,
            EmbedderStack::from_parts(embedder.clone(), None),
        );
        let index_dir = fixture.root.join(".ee/index");
        let rebuilt = crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
            workspace_path: fixture.root.clone(),
            database_path: Some(fixture.root.join(".ee/ee.db")),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
        })
        .unwrap();
        assert_eq!(
            rebuilt.status,
            crate::core::index::IndexRebuildStatus::Success
        );
        assert_eq!(rebuilt.documents_total, 128);
        assert!(rebuilt.errors.is_empty());
        assert!(
            embedder.calls.load(Ordering::Relaxed) >= 128,
            "the fixture counter must observe initial corpus embedding"
        );
        // Pin both sides: the pre-fix global check refuses this very index,
        // while the producer that built it is admitted by the search reader.
        let mismatch = crate::core::index::validate_index_corpus_compatibility(&index_dir)
            .expect_err("the 128d process default must disagree with the stored 256d producer");
        assert!(
            mismatch.contains("embedding dimensions cannot be mixed"),
            "{mismatch}"
        );
        crate::core::index::validate_index_corpus_compatibility_with_embedder(
            &index_dir,
            embedder.as_ref(),
        )
        .expect("workspace producer matches the published index");
        let initial_generation = fixture
            .db
            .get_workspace_generation(&fixture.workspace)
            .unwrap()
            .expect("initial source generation");
        assert_eq!(
            crate::core::index::published_index_generation(&index_dir),
            Some(initial_generation),
            "daemon keys and prefetch retain a foreign-default workspace's watermark"
        );

        let remembered = fixture.remember_deferred(BODY);
        let jobs_before = fixture
            .db
            .list_search_index_jobs(&fixture.workspace, None)
            .unwrap();
        assert_eq!(jobs_before.len(), 1);
        assert_eq!(Some(&jobs_before[0].id), remembered.index_job_id.as_ref());
        assert_eq!(jobs_before[0].status, "pending");
        let calls_before = embedder.calls.load(Ordering::Relaxed);
        let mut options = fixture.options();
        options.query = BODY.to_owned();
        options.source_mode = SearchSourceMode::SemanticOnly;
        options.strict_source_mode = true;

        // Exercise the ordinary pre-read reconciliation path, with the full
        // rebuild ceiling exceeded and a single durable document change.
        let run = run_search_with_performance(&options).unwrap();
        // Reconciliation failures are only logged, so carry what the report
        // says about freshness into the failure (bd-q1e4y: the first run
        // ranked fixture memory 1 first, i.e. the delta was not scored).
        assert_eq!(
            run.report.results[0].doc_id,
            remembered.memory_id.to_string(),
            "delta not ranked first; embedder calls since remember: {}; index_freshness: {:?}; degraded: {:?}; {}",
            embedder.calls.load(Ordering::Relaxed) - calls_before,
            run.report.index_freshness,
            run.report
                .degraded
                .iter()
                .map(|entry| entry.code.as_str())
                .collect::<Vec<_>>(),
            delta_publication_probe(&fixture, &options, embedder.as_ref())
        );
        assert!(run.report.results[0].fast_score.is_some());
        assert_eq!(
            run.report.source_mode_applied,
            SearchSourceMode::SemanticOnly
        );
        assert!(!run.report.source_mode_fallback);
        assert!(run.report.index_freshness.is_none());
        assert!(!run.report.degraded.iter().any(|entry| matches!(
            entry.code.as_str(),
            "index_missing" | "search_index_stale" | "search_live_snapshot_lexical"
        )));
        let delta_calls = embedder.calls.load(Ordering::Relaxed) - calls_before;
        assert!(
            (1..=8).contains(&delta_calls),
            "one changed document plus query inference must not reembed 128 unchanged \
             documents: observed {delta_calls} calls"
        );
        let jobs_after = fixture
            .db
            .list_search_index_jobs(&fixture.workspace, None)
            .unwrap();
        assert_eq!(jobs_after.len(), 1);
        assert_eq!(jobs_after[0].id, jobs_before[0].id);
        assert_eq!(jobs_after[0].status, "completed");
        // Vacuum and crash recovery must preserve the displaced workspace
        // generation even though the process-wide model cannot query it.
        let retention = crate::core::index::retention_gc::plan(&index_dir, 2).unwrap();
        let previous = retention
            .kept()
            .find(|entry| entry.generation == Some(initial_generation))
            .expect("vacuum must retain the displaced workspace generation");
        crate::core::index::validate_index_corpus_compatibility_with_embedder(
            &previous.path,
            embedder.as_ref(),
        )
        .expect("retained workspace snapshot is still queryable");
        let status = crate::core::index::get_index_status(&IndexStatusOptions {
            workspace_path: fixture.root.clone(),
            database_path: Some(fixture.root.join(".ee/ee.db")),
            index_dir: Some(index_dir.clone()),
        })
        .unwrap();
        assert_eq!(status.health, IndexHealth::Ready);
        assert_eq!(status.index_generation, status.db_generation);
        assert_eq!(status.index_document_count, Some(129));
        let published = crate::core::index::published_index_generation(&index_dir);
        assert_eq!(published, status.index_generation);
        assert_ne!(
            published,
            Some(initial_generation),
            "foreign-default workspaces must not collapse distinct publications to None"
        );

        options.query = RETAINED_BODY.to_owned();
        let retained = run_search_with_performance(&options).unwrap();
        assert_eq!(retained.report.results[0].doc_id, retained_memory);
        assert_eq!(
            retained.report.source_mode_applied,
            SearchSourceMode::SemanticOnly
        );
        assert!(!retained.report.source_mode_fallback);
        assert!(retained.report.index_freshness.is_none());

        // Removing process-model authority must not relax structural,
        // security or source-watermark admission for any hint consumer.
        let metadata_path = index_dir.join("meta.json");
        let original_bytes = std::fs::read(&metadata_path).unwrap();
        let original: serde_json::Value = serde_json::from_slice(&original_bytes).unwrap();
        for (field, value) in [
            ("schema", serde_json::json!("unsupported-index-schema")),
            ("corpusRevision", serde_json::json!("foreign-corpus")),
            ("evidenceSecurityPolicyEpoch", serde_json::json!(u64::MAX)),
            ("sourceGeneration", serde_json::json!(u64::MAX)),
            ("documentCount", serde_json::json!(0)),
        ] {
            let mut invalid = original.clone();
            invalid[field] = value;
            std::fs::write(&metadata_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert_eq!(
                crate::core::index::published_index_generation(&index_dir),
                None,
                "published watermark must reject invalid {field}"
            );
        }
        std::fs::write(&metadata_path, original_bytes).unwrap();
        assert_eq!(
            crate::core::index::published_index_generation(&index_dir),
            published
        );
    });
    println!("{SENTINEL}");
}

/// bd-q1e4y: the one red of the test above ranked fixture memory 1 first,
/// meaning the delta was not scored. The pre-read reconciliation only logs a
/// failure and promises "truthful stale-index reporting" instead. This pins
/// that promise for an existing generation with one pending delta whose
/// publication fails. The search must either refuse, or return results that
/// say the generation is stale. Serving the older generation as current is
/// the fail-open case.
#[cfg(unix)]
#[test]
fn strict_semantic_search_reports_a_stale_generation_when_delta_publication_is_cancelled() {
    with_test_search_timeouts(Duration::from_secs(300), || {
        let fixture = Fixture::new();
        fixture.memory(
            1,
            "The archivedcobalt upgrade requires an immutable rollback plan.",
        );
        for number in 2..=128_u128 {
            fixture.memory(number, &format!("Earlier deployment note {number}."));
        }
        let embedder = Arc::new(FirstPublicationEmbedder::new(false));
        let _selection = crate::core::index::install_test_workspace_embedder(
            &fixture.workspace,
            EmbedderStack::from_parts(embedder.clone(), None),
        );
        let rebuilt = crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
            workspace_path: fixture.root.clone(),
            database_path: Some(fixture.root.join(".ee/ee.db")),
            index_dir: Some(fixture.root.join(".ee/index")),
            dry_run: false,
        })
        .unwrap();
        assert_eq!(
            rebuilt.status,
            crate::core::index::IndexRebuildStatus::Success
        );
        let remembered = fixture.remember_deferred(BODY);
        let mut options = fixture.options();
        options.query = BODY.to_owned();
        options.source_mode = SearchSourceMode::SemanticOnly;
        options.strict_source_mode = true;

        // Reconciliation runs before the query is embedded, so the first
        // embedding after this point is the delta's publication.
        embedder.cancel_next.store(true, Ordering::Relaxed);
        let outcome = run_search_with_performance(&options);
        assert!(
            !embedder.cancel_next.load(Ordering::Relaxed),
            "nothing was embedded after the cancel was armed"
        );
        let interrupted = fixture
            .db
            .list_search_index_jobs(&fixture.workspace, None)
            .unwrap();
        assert_eq!(interrupted.len(), 1);
        assert!(
            !matches!(interrupted[0].status.as_str(), "running" | "completed"),
            "the cancel must interrupt the delta's publication, or this test observes \
             nothing; job status {:?}",
            interrupted[0].status
        );
        if let Ok(run) = &outcome {
            let codes = run
                .report
                .degraded
                .iter()
                .map(|entry| entry.code.as_str())
                .collect::<Vec<_>>();
            let stale_reported = run
                .report
                .index_freshness
                .as_ref()
                .is_some_and(|freshness| freshness.stale)
                || codes.contains(&"search_index_stale");
            assert!(
                stale_reported,
                "an unpublished delta was served as current: top hit {:?}, index_freshness \
                 {:?}, degraded {codes:?}, source mode {:?}, fallback {}",
                run.report.results.first().map(|hit| hit.doc_id.as_str()),
                run.report.index_freshness,
                run.report.source_mode_applied,
                run.report.source_mode_fallback
            );
        }

        // The interrupted job is still there, so the next search publishes it.
        let retried = run_search_with_performance(&options).unwrap();
        assert_eq!(
            retried.report.results[0].doc_id,
            remembered.memory_id.to_string(),
            "the retry did not publish the interrupted delta; index_freshness: {:?}; degraded: {:?}; {}",
            retried.report.index_freshness,
            retried
                .report
                .degraded
                .iter()
                .map(|entry| entry.code.as_str())
                .collect::<Vec<_>>(),
            delta_publication_probe(&fixture, &options, embedder.as_ref())
        );
        assert!(!retried.report.source_mode_fallback);
        assert!(retried.report.index_freshness.is_none());
        let completed = fixture
            .db
            .list_search_index_jobs(&fixture.workspace, None)
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].status, "completed");
    });
}

#[test]
fn first_publication_without_durable_jobs_keeps_the_source_only_fallback() {
    let fixture = Fixture::new();
    let memory = fixture.memory(850, BODY);
    let _selection = crate::core::index::install_test_hash_workspace_embedder(&fixture.workspace);
    let source_generation = fixture
        .db
        .get_workspace_generation(&fixture.workspace)
        .unwrap();
    let mut options = fixture.options();
    options.source_mode = SearchSourceMode::Hybrid;
    let run = run_search_with_performance(&options).unwrap();
    complete_lexical(&run.report);
    assert_eq!(run.report.results[0].doc_id, memory);
    assert!(
        fixture
            .db
            .list_search_index_jobs(&fixture.workspace, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture
            .db
            .get_workspace_generation(&fixture.workspace)
            .unwrap(),
        source_generation
    );
    assert!(!fixture.root.join(".ee/index").exists());
}

#[cfg(unix)]
#[test]
fn first_publication_refuses_a_corpus_above_the_interactive_limit() {
    let fixture = Fixture::new();
    for number in 1..=SEARCH_INDEX_AUTO_RECONCILE_MAX_DOCUMENTS {
        fixture.memory(
            u128::from(number),
            &format!("Earlier deployment note {number}."),
        );
    }
    let remembered = fixture.remember_deferred(BODY);
    let jobs = fixture
        .db
        .list_search_index_jobs(&fixture.workspace, None)
        .unwrap();
    let embedder = Arc::new(FirstPublicationEmbedder::new(false));
    let _selection = crate::core::index::install_test_workspace_embedder(
        &fixture.workspace,
        EmbedderStack::from_parts(embedder.clone(), None),
    );
    let mut options = fixture.options();
    options.source_mode = SearchSourceMode::SemanticOnly;
    options.strict_source_mode = true;
    assert!(matches!(
        run_search_with_performance(&options),
        Err(SearchError::NoIndex)
    ));
    assert_eq!(
        fixture
            .db
            .list_search_index_jobs(&fixture.workspace, None)
            .unwrap(),
        jobs
    );
    assert_eq!(Some(&jobs[0].id), remembered.index_job_id.as_ref());
    assert_eq!(jobs[0].status, "pending");
    assert_eq!(embedder.calls.load(Ordering::Relaxed), 0);
    assert!(!fixture.root.join(".ee/index").exists());
}

#[cfg(unix)]
#[test]
fn cancelled_first_publication_preserves_the_job_for_the_next_search() {
    with_test_search_timeouts(Duration::from_secs(300), || {
        let fixture = Fixture::new();
        let remembered = fixture.remember_deferred(BODY);
        let embedder = Arc::new(FirstPublicationEmbedder::new(true));
        let _selection = crate::core::index::install_test_workspace_embedder(
            &fixture.workspace,
            EmbedderStack::from_parts(embedder.clone(), None),
        );
        let mut options = fixture.options();
        options.query = BODY.to_owned();
        options.source_mode = SearchSourceMode::SemanticOnly;
        options.strict_source_mode = true;

        assert!(matches!(
            run_search_with_performance(&options),
            Err(SearchError::NoIndex)
        ));
        assert!(
            embedder.calls.load(Ordering::Relaxed) > 0,
            "cancel inside actual publication"
        );
        let index_dir = fixture.root.join(".ee/index");
        assert!(!index_dir.exists() || index_dir_is_plain_empty(&index_dir));
        let interrupted = fixture
            .db
            .list_search_index_jobs(&fixture.workspace, None)
            .unwrap();
        assert_eq!(interrupted.len(), 1);
        assert_eq!(Some(&interrupted[0].id), remembered.index_job_id.as_ref());
        assert!(!matches!(
            interrupted[0].status.as_str(),
            "running" | "completed"
        ));

        let retried = run_search_with_performance(&options).unwrap();
        assert_eq!(
            retried.report.results[0].doc_id,
            remembered.memory_id.to_string()
        );
        assert_eq!(
            retried.report.source_mode_applied,
            SearchSourceMode::SemanticOnly
        );
        assert!(!retried.report.source_mode_fallback);
        assert!(retried.report.index_freshness.is_none());
        let completed = fixture
            .db
            .list_search_index_jobs(&fixture.workspace, None)
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(Some(&completed[0].id), remembered.index_job_id.as_ref());
        assert_eq!(completed[0].status, "completed");
    });
}

#[cfg(unix)]
#[test]
fn first_publication_never_reclassifies_rejected_or_retained_index_bytes() {
    for layout in ["orphaned", "corrupt", "nested", "retained"] {
        let fixture = Fixture::new();
        fixture.remember_deferred(BODY);
        let _selection =
            crate::core::index::install_test_hash_workspace_embedder(&fixture.workspace);
        let index_dir = fixture.root.join(".ee/index");
        let inspected = if layout == "retained" {
            fixture.root.join(".ee/index.previous")
        } else {
            index_dir.clone()
        };
        std::fs::create_dir(&inspected).unwrap();
        match layout {
            "orphaned" => std::fs::write(inspected.join("fast.idx"), b"unadmitted-vector").unwrap(),
            "nested" => {
                std::fs::create_dir(inspected.join("lexical")).unwrap();
                std::fs::write(inspected.join("lexical/fragment"), b"unadmitted-lexical").unwrap();
            }
            _ => std::fs::write(inspected.join("meta.json"), b"unadmitted-metadata").unwrap(),
        }
        let before = fixture.state();
        let bytes = index_file_hashes(&inspected);
        let mut options = fixture.options();
        options.source_mode = SearchSourceMode::Hybrid;
        crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
            reconcile_search_index_before_read_with_cx(&cx, &options, false).await;
        })
        .unwrap();
        assert_eq!(
            fixture.state(),
            before,
            "{layout} must preserve queued jobs and source state"
        );
        assert_eq!(
            index_file_hashes(&inspected),
            bytes,
            "{layout} must preserve derived bytes"
        );
        if layout == "retained" {
            assert!(!index_dir.exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn lexical_search_reads_deferred_remember_without_publishing_at_default_or_quality_speed() {
    with_test_search_timeouts(Duration::from_secs(300), || {
        for stale_index in [false, true] {
            let fixture = Fixture::new();
            let index_dir = fixture.root.join(".ee/index");
            let _embedder = if stale_index {
                fixture.memory(802, "Previously published deployment guidance.");
                Some(publish_store_index(
                    &fixture.root,
                    &fixture.root.join(".ee/ee.db"),
                    &index_dir,
                    &fixture.workspace,
                ))
            } else {
                None
            };
            let remembered = fixture.remember_deferred(BODY);
            let jobs_before = fixture
                .db
                .list_search_index_jobs(&fixture.workspace, None)
                .unwrap();
            assert!(jobs_before.iter().any(|job| {
                Some(&job.id) == remembered.index_job_id.as_ref() && job.status == "pending"
            }));
            let index_before = index_file_hashes(&index_dir);

            for speed in [SpeedMode::Default, SpeedMode::Quality] {
                let mut options = fixture.options();
                options.speed = speed;
                options.strict_source_mode = true;
                // Exercise the ordinary entry point, including its nonzero
                // reconciliation budget; a retrieval-only helper would hide
                // the eager embedding/index-publication regression.
                let run = run_search_with_performance(&options).unwrap();
                complete_lexical(&run.report);
                assert!(!run.report.source_mode_fallback);
                assert_eq!(
                    run.report.results[0].doc_id,
                    remembered.memory_id.to_string()
                );
                assert!(
                    run.performance
                        .timings()
                        .all(|(name, _)| name != "search::embedderPrepare")
                );
                if stale_index {
                    assert!(run.report.index_freshness.as_ref().unwrap().stale);
                } else {
                    assert!(
                        run.report
                            .degraded
                            .iter()
                            .any(|entry| entry.code == "index_missing")
                    );
                    assert!(!index_dir.exists());
                }
                assert_eq!(
                    fixture
                        .db
                        .list_search_index_jobs(&fixture.workspace, None)
                        .unwrap(),
                    jobs_before,
                    "lexical search must not claim, complete, or fail the deferred job"
                );
                assert_eq!(index_file_hashes(&index_dir), index_before);
            }
        }
    });
}

#[cfg(unix)]
#[test]
fn persisting_lexical_pack_reads_deferred_remember_without_publishing_index() {
    with_test_search_timeouts(Duration::from_secs(300), || {
        let fixture = Fixture::new();
        fixture.memory(803, "Previously published deployment guidance.");
        let index_dir = fixture.root.join(".ee/index");
        let _embedder = publish_store_index(
            &fixture.root,
            &fixture.root.join(".ee/ee.db"),
            &index_dir,
            &fixture.workspace,
        );
        let remembered = fixture.remember_deferred(BODY);
        let jobs_before = fixture
            .db
            .list_search_index_jobs(&fixture.workspace, None)
            .unwrap();
        let index_before = index_file_hashes(&index_dir);

        for speed in [SpeedMode::Default, SpeedMode::Quality] {
            let mut options = fixture.pack_options();
            options.speed = speed;
            options.strict_source_mode = true;
            // This caller authorizes requested repair as well as persisting
            // the pack. Explicit lexical retrieval still must not embed.
            options.persist_pack = true;
            let packed = crate::core::context::run_context_pack_with_performance(
                &options,
                crate::pack::PACK_COMMAND,
            )
            .unwrap();
            assert!(packed.response.success);
            assert!(
                packed
                    .response
                    .data
                    .pack
                    .items
                    .iter()
                    .any(|item| { item.memory_id.to_string() == remembered.memory_id.to_string() })
            );
            assert!(
                packed
                    .response
                    .data
                    .degraded
                    .iter()
                    .any(|entry| entry.code == "search_live_snapshot_lexical")
            );
            for timing in packed.performance["data"]["timings"].as_array().unwrap() {
                assert_ne!(timing["name"], "embedderPrepare");
            }
            assert_eq!(
                fixture
                    .db
                    .list_search_index_jobs(&fixture.workspace, None)
                    .unwrap(),
                jobs_before,
                "persisting a lexical pack must leave the index queue unchanged"
            );
            assert_eq!(index_file_hashes(&index_dir), index_before);
        }
    });
}

#[cfg(unix)]
#[test]
fn readonly_hybrid_search_and_pack_skip_model_preparation_for_complete_live_sources() {
    for stale_index in [false, true] {
        let fixture = Fixture::new();
        let _embedder = if stale_index {
            fixture.memory(800, "Previously published deployment guidance.");
            Some(publish_store_index(
                &fixture.root,
                &fixture.root.join(".ee/ee.db"),
                &fixture.root.join(".ee/index"),
                &fixture.workspace,
            ))
        } else {
            None
        };
        let memory = fixture.memory(801, BODY);
        fixture
            .db
            .insert_search_index_job(
                "sidx_00000000000000000000000801",
                &crate::db::CreateSearchIndexJobInput {
                    workspace_id: fixture.workspace.clone(),
                    job_type: crate::db::SearchIndexJobType::SingleDocument,
                    document_source: Some("memory".to_owned()),
                    document_id: Some(memory.clone()),
                    documents_total: 1,
                },
            )
            .unwrap();
        let source_before = fixture.state();
        let index_before = index_file_hashes(&fixture.root.join(".ee/index"));
        let mut options = fixture.options();
        options.source_mode = SearchSourceMode::Hybrid;
        let run = crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
            run_search_with_performance_and_filters_with_cx_and_reconcile_timeout(
                &cx,
                &options,
                None,
                &[],
                Duration::ZERO,
                Duration::from_secs(60),
                None,
                true,
                false,
                false,
            )
            .await
        })
        .unwrap()
        .unwrap();
        complete_lexical(&run.report);
        assert_eq!(run.report.results[0].doc_id, memory);
        assert!(
            run.performance
                .timings()
                .all(|(name, _)| name != "search::embedderPrepare"),
            "complete live-source search must never enter model preparation"
        );
        assert_eq!(fixture.state(), source_before);
        assert_eq!(
            index_file_hashes(&fixture.root.join(".ee/index")),
            index_before
        );

        let mut pack_options = fixture.pack_options();
        pack_options.source_mode = SearchSourceMode::Hybrid;
        let packed = crate::core::context::run_context_pack_with_performance(
            &pack_options,
            crate::pack::PACK_COMMAND,
        )
        .unwrap();
        assert!(packed.response.success);
        assert!(
            packed
                .response
                .data
                .pack
                .items
                .iter()
                .any(|item| item.memory_id.to_string() == memory)
        );
        assert!(
            packed
                .response
                .data
                .degraded
                .iter()
                .any(|entry| { entry.code == "search_live_snapshot_lexical" })
        );
        for timing in packed.performance["data"]["timings"].as_array().unwrap() {
            assert_ne!(timing["name"], "embedderPrepare");
        }
        assert_eq!(fixture.state(), source_before);
        assert_eq!(
            index_file_hashes(&fixture.root.join(".ee/index")),
            index_before
        );
        if !stale_index {
            assert!(!fixture.root.join(".ee/index").exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn readonly_hybrid_pack_reuses_complete_model_free_daemon_handoff() {
    for stale_index in [false, true] {
        let fixture = Fixture::new();
        let index = fixture.root.join(".ee/index");
        let _embedder = if stale_index {
            fixture.memory(804, "Previously published deployment guidance.");
            Some(publish_store_index(
                &fixture.root,
                &fixture.root.join(".ee/ee.db"),
                &index,
                &fixture.workspace,
            ))
        } else {
            None
        };
        let remembered = fixture.remember_deferred(BODY);
        let source_before = fixture.state();
        let index_before = index_file_hashes(&index);
        let registry_before = fixture
            .db
            .list_model_registry_entries(&fixture.workspace)
            .unwrap();
        let packs_before = fixture.db.count_table_rows("pack_records").unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let provider = |options: &SearchOptions, require_cached_local: bool| {
            assert!(require_cached_local);
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let handoff = run_pack_search_with_runtime_and_cached_local_embedder(
                options,
                None,
                None,
                require_cached_local,
            )
            .map_err(|error| SearchDegradation::daemon_fallback(&error.to_string()))?;
            complete_lexical(&handoff.report);
            assert!(!handoff.has_cached_local_embedder());
            assert!(handoff.supports_read_only_pack());
            assert!(handoff.matches_request(options));
            let reader =
                DbConnection::open_file_read_only(options.resolve_database_path()).unwrap();
            assert!(handoff.snapshot_matches(options, &reader));
            // Exercise the same typed wire boundary as the daemon client.
            Ok(PackSearchHandoff::from_value(serde_json::to_value(handoff).unwrap()).unwrap())
        };

        for speed in [SpeedMode::Default, SpeedMode::Quality] {
            let mut options = fixture.pack_options();
            options.source_mode = SearchSourceMode::Hybrid;
            options.speed = speed;
            let packed = crate::core::context::run_context_pack_with_search_provider(
                &options,
                crate::pack::PACK_COMMAND,
                &provider,
            )
            .unwrap();
            assert!(packed.response.success);
            assert!(
                packed
                    .response
                    .data
                    .pack
                    .items
                    .iter()
                    .any(|item| { item.memory_id.to_string() == remembered.memory_id.to_string() })
            );
            assert!(
                packed
                    .response
                    .data
                    .degraded
                    .iter()
                    .any(|entry| entry.code == "search_live_snapshot_lexical")
            );
            assert!(
                !packed
                    .response
                    .data
                    .degraded
                    .iter()
                    .any(|entry| entry.code == "daemon_search_fallback")
            );
            let timings = packed.performance["data"]["timings"].as_array().unwrap();
            assert!(
                timings
                    .iter()
                    .any(|timing| timing["name"] == "daemonRetrieval")
            );
            assert!(
                timings
                    .iter()
                    .all(|timing| timing["name"] != "embedderPrepare")
            );
            assert_eq!(fixture.state(), source_before);
            assert_eq!(index_file_hashes(&index), index_before);
            assert_eq!(
                fixture
                    .db
                    .list_model_registry_entries(&fixture.workspace)
                    .unwrap(),
                registry_before
            );
            assert_eq!(
                fixture.db.count_table_rows("pack_records").unwrap(),
                packs_before
            );
            assert!(!fixture.root.join(".ee/pack-slots").exists());
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        if !stale_index {
            assert!(!index.exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn cached_only_pack_refuses_model_requiring_requests_before_preparation() {
    let fixture = Fixture::new();
    fixture.memory(805, BODY);
    for refusal in [
        "strict",
        "semantic",
        "historical",
        "tombstoned",
        "published",
    ] {
        let mut options = fixture.options();
        options.source_mode = SearchSourceMode::Hybrid;
        match refusal {
            "strict" => options.strict_source_mode = true,
            "semantic" => options.source_mode = SearchSourceMode::SemanticOnly,
            "historical" => options.as_of = Some(Utc::now()),
            "tombstoned" => options.include_tombstoned = true,
            "published" => {}
            _ => unreachable!(),
        }
        let index = fixture.root.join(".ee/index");
        let _embedder = (refusal == "published").then(|| {
            publish_store_index(
                &fixture.root,
                &fixture.root.join(".ee/ee.db"),
                &index,
                &fixture.workspace,
            )
        });
        let source_before = fixture.state();
        let index_before = index_file_hashes(&index);
        let registry_before = fixture
            .db
            .list_model_registry_entries(&fixture.workspace)
            .unwrap();
        let result =
            run_pack_search_with_runtime_and_cached_local_embedder(&options, None, None, true);
        assert!(
            matches!(result, Err(SearchError::Index(message))
                if message.contains("already-loaded local model or a complete current lexical source snapshot")),
            "{refusal} must stop before preparing any model"
        );
        assert_eq!(fixture.state(), source_before);
        assert_eq!(index_file_hashes(&index), index_before);
        assert_eq!(
            fixture
                .db
                .list_model_registry_entries(&fixture.workspace)
                .unwrap(),
            registry_before
        );
    }
}

#[cfg(unix)]
#[test]
fn prepared_live_generation_is_bound_to_its_query_and_source_snapshot() {
    let fixture = Fixture::new();
    fixture.memory(810, BODY);
    let reader = fixture.reader();
    let mut options = fixture.options();
    options.source_mode = SearchSourceMode::Hybrid;
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        for changed_source in [false, true] {
            let snapshot =
                rule_admission::memory_revisions::RevisionReadSnapshot::begin_or_borrow(&reader)
                    .unwrap();
            let prepared = prepare_model_free_search_generation(&cx, &options, &reader)
                .await
                .unwrap()
                .expect("bounded unpublished source prepares a live retrieval");
            assert!(matches!(&prepared, PreparedSearchGeneration::Live(_)));
            let mut requested = options.clone();
            if changed_source {
                snapshot.finish().unwrap();
                fixture.memory(811, "A source write after the prepared snapshot.");
            } else {
                requested.query = "a different retrieval request".to_owned();
                // Borrow the existing snapshot through the retrieval call.
                let rejected = run_context_search_with_prepared_generation(
                    &cx,
                    &requested,
                    &reader,
                    None,
                    None,
                    Deterministic::from_seed(0).shared_child("search.rerank"),
                    None,
                    Some(prepared),
                )
                .await;
                assert!(matches!(rejected, Err(SearchError::Index(message))
                    if message.contains("request and pinned source snapshot")));
                snapshot.finish().unwrap();
                continue;
            }
            let rejected = run_context_search_with_prepared_generation(
                &cx,
                &requested,
                &reader,
                None,
                None,
                Deterministic::from_seed(0).shared_child("search.rerank"),
                None,
                Some(prepared),
            )
            .await;
            assert!(matches!(rejected, Err(SearchError::Index(message))
                if message.contains("request and pinned source snapshot")));
        }
    })
    .unwrap();
}

#[cfg(unix)]
#[test]
fn model_free_live_preparation_preserves_retained_generation_and_producer_admission() {
    for retained_only in [false, true] {
        let fixture = Fixture::new();
        fixture.memory(830, "The previously published source generation.");
        let index_dir = fixture.root.join(".ee/index");
        let _embedder = publish_store_index(
            &fixture.root,
            &fixture.root.join(".ee/ee.db"),
            &index_dir,
            &fixture.workspace,
        );
        fixture.memory(831, BODY);
        let selected_index = if retained_only {
            let retained = index_dir.with_file_name("index.previous");
            std::fs::rename(&index_dir, &retained).unwrap();
            retained
        } else {
            index_dir.clone()
        };
        let source_before = fixture.state();
        let index_before = index_file_hashes(&selected_index);
        crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
            let reader = fixture.reader();
            let snapshot =
                rule_admission::memory_revisions::RevisionReadSnapshot::begin_or_borrow(&reader)
                    .unwrap();
            let mut options = fixture.options();
            options.source_mode = SearchSourceMode::Hybrid;
            let prepared = prepare_model_free_search_generation(&cx, &options, &reader)
                .await
                .unwrap()
                .expect("complete stale source admits the matching producer");
            let PreparedSearchGeneration::Live(live) = prepared else {
                panic!("nonempty stale source requires a complete live retrieval");
            };
            assert_eq!(live.index_dir, selected_index);
            assert!(!live.unpublished_source);
            drop(live);

            let _mismatched_producer = crate::core::index::install_test_workspace_embedder(
                &fixture.workspace,
                crate::search::EmbedderStack::from_parts(
                    Arc::new(crate::search::HashEmbedder::jl_384(17)),
                    None,
                ),
            );
            assert!(
                prepare_model_free_search_generation(&cx, &options, &reader)
                    .await
                    .unwrap()
                    .is_none(),
                "passive producer mismatch must retain ordinary model admission"
            );
            snapshot.finish().unwrap();
            assert_eq!(fixture.state(), source_before);
            assert_eq!(index_file_hashes(&selected_index), index_before);
            if retained_only {
                assert!(!index_dir.exists());
            }
        })
        .unwrap();
    }
}

#[cfg(unix)]
#[test]
fn model_free_live_preparation_preserves_strict_semantic_and_historical_modes() {
    let fixture = Fixture::new();
    fixture.memory(820, BODY);
    let reader = fixture.reader();
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        let snapshot =
            rule_admission::memory_revisions::RevisionReadSnapshot::begin_or_borrow(&reader)
                .unwrap();
        for case in 0..5 {
            let mut options = fixture.options();
            options.source_mode = SearchSourceMode::Hybrid;
            match case {
                0 => options.strict_source_mode = true,
                1 => options.source_mode = SearchSourceMode::SemanticOnly,
                2 => options.as_of = Some(Utc::now()),
                3 => options.include_tombstoned = true,
                _ => options.source_mode = SearchSourceMode::LexicalOnly,
            }
            assert!(
                prepare_model_free_search_generation(&cx, &options, &reader)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        snapshot.finish().unwrap();
    })
    .unwrap();
}

#[test]
fn readonly_pack_can_select_unlinked_cass_evidence_before_first_publication() {
    use crate::core::context::run_context_pack;
    let fixture = Fixture::new();
    let evidence = fixture.evidence();
    let before = fixture.state();
    let mut options = fixture.pack_options();
    options.source_mode = SearchSourceMode::Hybrid;
    let response = run_context_pack(&options).unwrap();
    assert!(response.success);
    assert!(
        response.data.pack.items.is_empty(),
        "native evidence is not a memory alias"
    );
    assert_eq!(response.data.pack.evidence_items.len(), 1);
    let item = &response.data.pack.evidence_items[0];
    assert_eq!(item.evidence_id, evidence);
    assert_eq!(item.content, BODY);
    assert!(!item.entity_revision.is_empty());
    assert!(
        response
            .data
            .degraded
            .iter()
            .any(|entry| entry.code == "search_live_snapshot_lexical")
    );
    assert_eq!(fixture.state(), before);
    assert!(!fixture.root.join(".ee/index").exists());
}

#[test]
fn strict_semantic_historical_and_tombstone_requests_never_claim_complete_current_fallback() {
    let fixture = Fixture::new();
    fixture.memory(220, BODY);
    let reader = fixture.reader();
    for mode in [SearchSourceMode::Hybrid, SearchSourceMode::SemanticOnly] {
        let mut options = fixture.options();
        options.source_mode = mode;
        options.strict_source_mode = true;
        assert!(matches!(
            run_search_with_read_connection(&options, &reader),
            Err(SearchError::NoIndex)
        ));
    }
    let mut historical = fixture.options();
    historical.as_of = Some(Utc::now());
    assert!(matches!(
        run_search_with_read_connection(&historical, &reader),
        Err(SearchError::NoIndex)
    ));
    let mut tombstones = fixture.options();
    tombstones.include_tombstoned = true;
    assert!(matches!(
        run_search_with_read_connection(&tombstones, &reader),
        Err(SearchError::NoIndex)
    ));
    let mut lexical = fixture.options();
    lexical.strict_source_mode = true;
    complete_lexical(&run_search_with_read_connection(&lexical, &reader).unwrap());
}

#[test]
fn complete_source_byte_ceiling_refuses_instead_of_searching_a_truncated_corpus() {
    let fixture = Fixture::new();
    let body = "x ".repeat(crate::models::MAX_CONTENT_BYTES / 2);
    fixture
        .db
        .with_transaction(|| {
            for number in 1000..1257 {
                fixture.memory(number, &body);
            }
            fixture.memory(1257, BODY);
            Ok(())
        })
        .unwrap();
    let generation = fixture
        .db
        .get_workspace_generation(&fixture.workspace)
        .unwrap();
    assert!(matches!(
        run_search_with_read_connection(&fixture.options(), &fixture.reader()),
        Err(SearchError::NoIndex)
    ));
    #[cfg(unix)]
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| {
        let fixture = &fixture;
        async move {
            let reader = fixture.reader();
            let snapshot =
                rule_admission::memory_revisions::RevisionReadSnapshot::begin_or_borrow(&reader)
                    .unwrap();
            let mut options = fixture.options();
            options.source_mode = SearchSourceMode::Hybrid;
            assert!(
                prepare_model_free_search_generation(&cx, &options, &reader)
                    .await
                    .unwrap()
                    .is_none()
            );
            snapshot.finish().unwrap();
        }
    })
    .unwrap();
    let mut options = fixture.options();
    options.source_mode = SearchSourceMode::Hybrid;
    assert!(matches!(
        run_pack_search_with_runtime_and_cached_local_embedder(&options, None, None, true),
        Err(SearchError::Index(message))
            if message.contains("complete current lexical source snapshot")
    ));
    assert_eq!(
        fixture
            .db
            .get_workspace_generation(&fixture.workspace)
            .unwrap(),
        generation
    );
    assert!(!fixture.root.join(".ee/index").exists());
}

#[test]
fn source_lifecycle_admission_still_excludes_sealed_retired_and_tombstoned_memory() {
    let fixture = Fixture::new();
    let visible = fixture.memory(230, BODY);
    let sealed = fixture.memory(231, BODY);
    let retired = fixture.memory(232, BODY);
    let tombstoned = fixture.memory(233, BODY);
    fixture
        .db
        .insert_memory_seal(
            &sealed,
            &format!("blake3:{}", "a".repeat(64)),
            "2020-01-01T00:00:00Z",
        )
        .unwrap();
    fixture
        .db
        .mark_memory_superseded(&retired, "2020-01-01T00:00:00Z")
        .unwrap();
    fixture.db.tombstone_memory(&tombstoned).unwrap();
    let before = fixture.state();
    let report = run_search_with_read_connection(&fixture.options(), &fixture.reader()).unwrap();
    complete_lexical(&report);
    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].doc_id, visible);
    assert_eq!(fixture.state(), before);
}

#[test]
fn source_fallback_borrows_the_callers_snapshot_across_concurrent_writes() {
    let fixture = Fixture::new();
    fixture.memory(240, "An unrelated earlier fact.");
    let reader = fixture.reader();
    reader.begin_read_snapshot().unwrap();
    reader.get_workspace_generation(&fixture.workspace).unwrap();
    let fresh = fixture.memory(241, BODY);
    let captured = run_search_with_read_connection(&fixture.options(), &reader).unwrap();
    complete_lexical(&captured);
    assert_eq!(captured.status, SearchStatus::NoResults);
    assert!(captured.results.is_empty());
    assert!(
        reader.begin_read_snapshot().is_err(),
        "borrowed transaction remains owned"
    );
    reader.rollback_read_snapshot().unwrap();
    let next = run_search_with_read_connection(&fixture.options(), &reader).unwrap();
    assert_eq!(next.results[0].doc_id, fresh);
    reader.begin_read_snapshot().unwrap();
    reader.rollback_read_snapshot().unwrap();
}

#[test]
fn calibration_review_authority_borrows_snapshots_without_losing_feedback() {
    let fixture = Fixture::new();
    let memory = fixture.memory(242, BODY);
    for ordinal in 0..MIN_SEARCH_SCORE_CALIBRATION_SAMPLES {
        fixture
            .db
            .insert_feedback_event(
                &format!("fb_{:026}", ordinal + 600),
                &crate::db::CreateFeedbackEventInput {
                    workspace_id: fixture.workspace.clone(),
                    target_type: "memory".to_owned(),
                    target_id: memory.clone(),
                    signal: "helpful".to_owned(),
                    weight: 1.0,
                    source_type: "outcome_observed".to_owned(),
                    source_id: None,
                    reason: None,
                    evidence_json: Some(r#"{"score":0.8,"groundTruthRelevance":0.6}"#.to_owned()),
                    session_id: None,
                },
            )
            .unwrap();
    }
    let database = fixture.root.join(".ee/ee.db");
    let reference_time = Utc::now() + chrono::Duration::hours(1);
    let load = |connection: Option<&DbConnection>| {
        search_score_calibration_for_workspace_cached(
            &fixture.root,
            Some(&database),
            connection,
            reference_time,
        )
    };
    let reader = fixture.reader();
    reader.begin_read_snapshot().unwrap();
    let before = load(Some(&reader));
    assert!(search_calibration_is_eligible_for_coverage(&before));
    assert_eq!(
        before.feedback_event_sample_count,
        MIN_SEARCH_SCORE_CALIBRATION_SAMPLES
    );
    let before_identity = before.input_identity().expect("complete initial authority");
    assert_eq!(load(None).input_identity().unwrap(), before_identity);
    fixture
        .db
        .insert_feedback_quarantine(
            "fq_00000000000000000000000600",
            &crate::db::CreateFeedbackQuarantineInput {
                workspace_id: fixture.workspace.clone(),
                source_id: "test-review-source".to_owned(),
                target_type: "memory".to_owned(),
                target_id: memory,
                signal: "harmful".to_owned(),
                weight: 1.0,
                source_type: "outcome_observed".to_owned(),
                proposed_event_id: None,
                recorded_at: "2026-01-01T00:00:00Z".to_owned(),
                reason: "review snapshot fixture".to_owned(),
                event_reason: None,
                evidence_json: None,
                session_id: None,
                raw_event_hash: format!("blake3:{}", "a".repeat(64)),
            },
        )
        .unwrap();
    assert_eq!(
        load(Some(&reader)).input_identity().unwrap(),
        before_identity,
        "borrowed source view remains pinned before the hold"
    );
    let fresh = load(None);
    assert_ne!(fresh.input_identity().unwrap(), before_identity);
    assert_eq!(
        fresh.data_json(),
        before.data_json(),
        "holds do not become calibration samples"
    );
    assert!(
        reader.begin_read_snapshot().is_err(),
        "caller still owns its snapshot"
    );
    reader.rollback_read_snapshot().unwrap();
    assert_eq!(load(Some(&reader)).input_identity(), fresh.input_identity());

    simulate_ddl(
        &fixture.db,
        "ALTER TABLE feedback_quarantine RENAME TO withheld_review_holds",
        "rename feedback_quarantine away",
    )
    .unwrap();
    for connection in [Some(&reader), None] {
        let unavailable = load(connection);
        assert!(
            unavailable.input_identity().is_none(),
            "unknown holds forbid reuse"
        );
        assert!(search_calibration_is_eligible_for_coverage(&unavailable));
        assert_eq!(
            unavailable.data_json(),
            before.data_json(),
            "unreadable hold metadata must not discard valid accepted feedback"
        );
    }
    reader.begin_read_snapshot().unwrap();
    reader.rollback_read_snapshot().unwrap();
}

#[test]
fn orphaned_index_bytes_are_not_reclassified_as_unpublished_sources() {
    let fixture = Fixture::new();
    fixture.memory(250, BODY);
    let index = fixture.root.join(".ee/index");
    std::fs::create_dir(&index).unwrap();
    std::fs::write(index.join("fast.idx"), b"orphaned-private-header").unwrap();
    assert!(run_search_with_read_connection(&fixture.options(), &fixture.reader()).is_err());
    #[cfg(unix)]
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| {
        let fixture = &fixture;
        async move {
            let reader = fixture.reader();
            let snapshot =
                rule_admission::memory_revisions::RevisionReadSnapshot::begin_or_borrow(&reader)
                    .unwrap();
            let mut options = fixture.options();
            options.source_mode = SearchSourceMode::Hybrid;
            assert!(
                prepare_model_free_search_generation(&cx, &options, &reader)
                    .await
                    .is_err()
            );
            snapshot.finish().unwrap();
        }
    })
    .unwrap();
    assert_eq!(
        std::fs::read(index.join("fast.idx")).unwrap(),
        b"orphaned-private-header"
    );
}

#[cfg(unix)]
#[test]
fn broken_or_ancestor_symlinks_cannot_enable_the_source_only_path() {
    let fixture = Fixture::new();
    fixture.memory(260, BODY);
    std::os::unix::fs::symlink(
        fixture.root.join("nonexistent-target"),
        fixture.root.join(".ee/index"),
    )
    .unwrap();
    assert!(matches!(
        run_search_with_read_connection(&fixture.options(), &fixture.reader()),
        Err(SearchError::IndexIncompatible(_))
    ));
    let actual = fixture.root.join("actual-index-parent");
    std::fs::create_dir(&actual).unwrap();
    std::os::unix::fs::symlink(&actual, fixture.root.join("redirected")).unwrap();
    let mut options = fixture.options();
    options.index_dir = Some(fixture.root.join("redirected/absent"));
    assert!(matches!(
        run_search_with_read_connection(&options, &fixture.reader()),
        Err(SearchError::IndexIncompatible(_))
    ));
    assert!(std::fs::read_dir(actual).unwrap().next().is_none());
}

#[test]
fn cancellation_before_source_projection_never_becomes_a_successful_empty_result() {
    let fixture = Fixture::new();
    fixture.memory(270, BODY);
    let reader = fixture.reader();
    let options = fixture.options();
    let result = with_search_root(|cx| async move {
        cx.cancel_with(
            asupersync::CancelKind::User,
            Some("unpublished fixture cancellation"),
        );
        run_search_with_read_connection_with_cx(&cx, &options, &reader).await
    });
    assert!(matches!(result, Err(SearchError::Cancelled(_))));
    assert!(!fixture.root.join(".ee/index").exists());
}

#[cfg(unix)]
#[test]
fn complete_unpublished_handoff_is_reusable_but_still_bound_to_its_request_and_snapshot() {
    let fixture = Fixture::new();
    let memory = fixture.memory(280, BODY);
    let mut options = fixture.options();
    options.source_mode = SearchSourceMode::Hybrid;
    let before = fixture.state();
    let handoff =
        run_pack_search_with_runtime_and_cached_local_embedder(&options, None, None, true).unwrap();
    complete_lexical(&handoff.report);
    assert!(
        handoff
            .report
            .degraded
            .iter()
            .any(|entry| entry.code == "index_missing")
    );
    assert!(handoff.can_reuse_for_pack());
    assert!(handoff.supports_read_only_pack());
    assert!(!handoff.has_cached_local_embedder());
    assert!(handoff.matches_request(&options));
    assert!(handoff.snapshot_matches(&options, &fixture.reader()));
    assert_eq!(handoff.report.results[0].doc_id, memory);
    let decoded = PackSearchHandoff::from_value(serde_json::to_value(&handoff).unwrap()).unwrap();
    assert!(decoded.can_reuse_for_pack());
    assert!(decoded.supports_read_only_pack());
    assert!(decoded.snapshot_matches(&options, &fixture.reader()));
    assert_eq!(
        fixture.state(),
        before,
        "retrieval-only handoff records no writes"
    );
    let mut different = options.clone();
    different.query = "another task".to_owned();
    assert!(!decoded.matches_request(&different));
    fixture.memory(281, "A newer source generation.");
    assert!(!decoded.snapshot_matches(&options, &fixture.reader()));
    assert!(!fixture.root.join(".ee/index").exists());
}

#[test]
fn source_only_handoff_cannot_hide_integrity_failures_or_invent_complete_execution() {
    let fixture = Fixture::new();
    fixture.memory(282, BODY);
    let handoff = run_pack_search(&fixture.options()).unwrap();
    assert!(handoff.can_reuse_for_pack());
    assert!(handoff.supports_read_only_pack());
    for code in [
        "index_corrupt",
        "index_incompatible",
        "search_index_degraded",
        "global_index_unavailable",
    ] {
        let mut failed = handoff.clone();
        failed.report.degraded.push(SearchDegradation {
            code: code.to_owned(),
            severity: "high".to_owned(),
            message: "Fixture integrity failure".to_owned(),
            repair: None,
        });
        assert!(!failed.can_reuse_for_pack(), "{code}");
        assert!(!failed.supports_read_only_pack(), "{code}");
    }
    let mut incomplete = handoff.clone();
    incomplete
        .report
        .degraded
        .retain(|entry| entry.code != "search_live_snapshot_lexical");
    assert!(!incomplete.can_reuse_for_pack());
    incomplete.report.degraded.push(SearchDegradation {
        code: "global_live_snapshot_lexical".to_owned(),
        severity: "warning".to_owned(),
        message: "Only the separate global snapshot was completely searched".to_owned(),
        repair: None,
    });
    assert!(
        !incomplete.can_reuse_for_pack(),
        "global completeness cannot certify an incomplete workspace snapshot"
    );
    let mut wrong_execution = handoff.clone();
    wrong_execution.report.source_mode_applied = SearchSourceMode::Hybrid;
    assert!(!wrong_execution.can_reuse_for_pack());
    assert!(!wrong_execution.supports_read_only_pack());
    for execution in [
        "fast",
        "quality",
        "rerank",
        "missing_lexical",
        "backend",
        "runtime",
    ] {
        let mut wrong_execution = handoff.clone();
        match execution {
            "fast" => wrong_execution.report.results[0].fast_score = Some(0.5),
            "quality" => wrong_execution.report.results[0].quality_score = Some(0.5),
            "rerank" => wrong_execution.report.results[0].rerank_score = Some(0.5),
            "missing_lexical" => wrong_execution.report.results[0].lexical_score = None,
            "backend" => wrong_execution.report.embed_backend = EmbedBackend::NeuralLocal,
            "runtime" => wrong_execution.report.rerank_runtime_available = true,
            _ => unreachable!(),
        }
        assert!(!wrong_execution.supports_read_only_pack(), "{execution}");
    }
    let mut incomplete = handoff.clone();
    incomplete.report.degraded.push(SearchDegradation {
        code: "search_live_snapshot_unavailable".to_owned(),
        severity: "warning".to_owned(),
        message: "The source scan did not cover the complete corpus".to_owned(),
        repair: None,
    });
    assert!(!incomplete.supports_read_only_pack());
    let mut strict = handoff.clone();
    strict.report.source_mode_requested = SearchSourceMode::Hybrid;
    strict.report.source_mode_fallback = true;
    strict.report.strict_source_mode = true;
    assert!(!strict.supports_read_only_pack());
    let mut unreported_fallback = handoff.clone();
    unreported_fallback.report.source_mode_requested = SearchSourceMode::Hybrid;
    assert!(!unreported_fallback.supports_read_only_pack());
    let mut semantic = handoff.clone();
    semantic.report.source_mode_requested = SearchSourceMode::SemanticOnly;
    assert!(!semantic.supports_read_only_pack());
    let mut historical = handoff.clone();
    historical.quality_reference_time = Some(Utc::now());
    assert!(!historical.supports_read_only_pack());
    let mut missing_witness = handoff.clone();
    missing_witness.snapshot.global_snapshot = None;
    assert!(!missing_witness.supports_read_only_pack());
    let mut failed_status = handoff;
    failed_status.report.status = SearchStatus::IndexError;
    assert!(!failed_status.can_reuse_for_pack());
    assert!(!failed_status.supports_read_only_pack());
}
