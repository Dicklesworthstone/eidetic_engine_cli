#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::db::{
    CreateEvidenceSpanInput, CreateMemoryInput, CreateProceduralRuleInput, CreateSessionInput,
    CreateWorkspaceInput, EvidenceProducerKind,
};
use crate::models::{EvidenceId, RuleId, SessionId};
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
            strict_scope: false,
        }
    }

    fn memory(&self, number: u128, body: &str) -> String {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_memory(
                &id,
                &CreateMemoryInput {
                    workspace_id: self.workspace.clone(),
                    content: body.to_owned(),
                    level: "semantic".to_owned(),
                    kind: "fact".to_owned(),
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
        let id = RuleId::from_uuid(uuid::Uuid::from_u128(201)).to_string();
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
                    maturity: "candidate".to_owned(),
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
                .any(|entry| entry.code == "global_live_snapshot_lexical")
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

#[test]
fn readonly_pack_can_select_unlinked_cass_evidence_before_first_publication() {
    use crate::core::context::run_context_pack;
    let fixture = Fixture::new();
    let evidence = fixture.evidence();
    let before = fixture.state();
    let options = fixture.pack_options();
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
fn orphaned_index_bytes_are_not_reclassified_as_unpublished_sources() {
    let fixture = Fixture::new();
    fixture.memory(250, BODY);
    let index = fixture.root.join(".ee/index");
    std::fs::create_dir(&index).unwrap();
    std::fs::write(index.join("fast.idx"), b"orphaned-private-header").unwrap();
    assert!(run_search_with_read_connection(&fixture.options(), &fixture.reader()).is_err());
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

#[test]
fn complete_unpublished_handoff_is_reusable_but_still_bound_to_its_request_and_snapshot() {
    let fixture = Fixture::new();
    let memory = fixture.memory(280, BODY);
    let options = fixture.options();
    let before = fixture.state();
    let handoff = run_pack_search(&options).unwrap();
    complete_lexical(&handoff.report);
    assert!(
        handoff
            .report
            .degraded
            .iter()
            .any(|entry| entry.code == "index_missing")
    );
    assert!(handoff.can_reuse_for_pack());
    assert!(handoff.matches_request(&options));
    assert!(handoff.snapshot_matches(&options, &fixture.reader()));
    assert_eq!(handoff.report.results[0].doc_id, memory);
    let decoded = PackSearchHandoff::from_value(serde_json::to_value(&handoff).unwrap()).unwrap();
    assert!(decoded.can_reuse_for_pack());
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
    let mut failed_status = handoff;
    failed_status.report.status = SearchStatus::IndexError;
    assert!(!failed_status.can_reuse_for_pack());
}
