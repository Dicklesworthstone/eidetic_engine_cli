#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;

fn isolate_global_store(test: &str, marker: &str) -> bool {
    if std::env::var_os(marker).is_some() {
        return false;
    }
    let root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(format!(
            "core::search::quality_tests::global_handoff_tests::{test}"
        ))
        .arg("--nocapture")
        .env(marker, "1")
        .env("XDG_DATA_HOME", root.path())
        .env("LOCALAPPDATA", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "global handoff child failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    true
}

fn include_global(fixture: &Fixture, enabled: bool) {
    std::fs::write(
        fixture.root.join(".ee/config.toml"),
        format!("[memory]\ninclude_global = {enabled}\n"),
    )
    .unwrap();
}

#[test]
fn global_handoff_tracks_absent_empty_unreturned_and_excluded_sources() {
    if isolate_global_store(
        "global_handoff_tracks_absent_empty_unreturned_and_excluded_sources",
        "EE_TEST_GLOBAL_HANDOFF_SOURCE_WITNESS",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let created = Utc::now() - chrono::Duration::days(1);
    fixture.memory(501, 1.0, 1.0, created);
    include_global(&fixture, true);
    let mut options = fixture.options();
    options.memory_scope = MemoryScope::Swarm;
    options.limit = 1;
    let paths = crate::core::global_store::default_global_store_paths_from_env().unwrap();

    let absent = run_pack_search(&options).unwrap();
    assert!(absent.can_reuse_for_pack());
    assert!(absent.snapshot_matches(&options, &fixture.db));
    assert!(
        !paths.database_path.exists(),
        "validation must not create a global store"
    );

    let global = GlobalQualityFixture::new(paths);
    assert!(!absent.snapshot_matches(&options, &fixture.db));
    let empty = run_pack_search(&options).unwrap();
    assert!(empty.can_reuse_for_pack());
    assert!(empty.snapshot_matches(&options, &fixture.db));
    let first = global.memory(502, created);
    let second = global.memory(503, created);
    assert!(!empty.snapshot_matches(&options, &fixture.db));

    let populated = run_pack_search(&options).unwrap();
    assert!(populated.snapshot_matches(&options, &fixture.db));
    assert_eq!(populated.report.results.len(), 1);
    let unreturned = [&first, &second]
        .into_iter()
        .find(|id| {
            populated
                .report
                .results
                .iter()
                .all(|hit| &hit.doc_id != *id)
        })
        .unwrap();
    global
        .db
        .execute_raw(&format!(
            "UPDATE memories SET confidence = 0.25 WHERE id = '{unreturned}'"
        ))
        .unwrap();
    assert!(
        !populated.snapshot_matches(&options, &fixture.db),
        "source changes outside top-k must invalidate the old candidate set"
    );

    // Future feedback does not affect today's harmful count. Its bytes still
    // belong to the complete ledger witness, including same-count edits.
    global.feedback(
        1,
        "memory",
        unreturned,
        "harmful",
        Utc::now() + chrono::Duration::days(1),
    );
    let before_feedback_edit = run_pack_search(&options).unwrap();
    let feedback_count = global.db.count_table_rows("feedback_events").unwrap();
    global
        .db
        .execute_raw("UPDATE feedback_events SET signal = 'helpful'")
        .unwrap();
    assert_eq!(
        global.db.count_table_rows("feedback_events").unwrap(),
        feedback_count
    );
    assert!(!before_feedback_edit.snapshot_matches(&options, &fixture.db));

    // A seal changes admission without changing the memory row itself.
    let before_seal = run_pack_search(&options).unwrap();
    let source_row = global.db.get_memory(unreturned).unwrap();
    global
        .db
        .insert_memory_seal(
            unreturned,
            &format!("blake3:{}", "a".repeat(64)),
            &Utc::now().to_rfc3339(),
        )
        .unwrap();
    assert_eq!(global.db.get_memory(unreturned).unwrap(), source_row);
    assert!(!before_seal.snapshot_matches(&options, &fixture.db));

    let enabled = run_pack_search(&options).unwrap();
    assert!(enabled.snapshot_matches(&options, &fixture.db));
    include_global(&fixture, false);
    assert!(!enabled.snapshot_matches(&options, &fixture.db));
    let excluded = run_pack_search(&options).unwrap();
    assert!(excluded.snapshot_matches(&options, &fixture.db));
    assert!(
        !global.paths.index_dir.exists(),
        "handoffs must not publish global indexes"
    );
    assert_eq!(global.db.count_table_rows("audit_log").unwrap(), 0);
    assert_eq!(global.db.count_table_rows("pack_records").unwrap(), 0);
}

#[test]
fn global_handoff_keeps_body_and_feedback_snapshot_through_rehydration() {
    if isolate_global_store(
        "global_handoff_keeps_body_and_feedback_snapshot_through_rehydration",
        "EE_TEST_GLOBAL_HANDOFF_PINNED_SOURCE",
    ) {
        return;
    }
    let fixture = Fixture::new();
    fixture.memory(601, 1.0, 1.0, Utc::now() - chrono::Duration::days(1));
    include_global(&fixture, true);
    let global = GlobalQualityFixture::new(
        crate::core::global_store::default_global_store_paths_from_env().unwrap(),
    );
    let global_id = global.memory(602, Utc::now() - chrono::Duration::days(1));
    global.feedback(
        1,
        "memory",
        &global_id,
        "harmful",
        Utc::now() - chrono::Duration::hours(1),
    );
    let options = fixture.options();
    let mut handoff = run_pack_search(&options).unwrap();
    assert!(
        handoff
            .report
            .results
            .iter()
            .any(|hit| hit.doc_id == global_id)
    );
    let before = serde_json::to_value(&handoff.report.results).unwrap();
    let validated = handoff.validate_snapshot(&options, &fixture.db).unwrap();

    global
        .db
        .execute_raw("UPDATE memories SET confidence = 0.25")
        .unwrap();
    global
        .db
        .execute_raw("UPDATE feedback_events SET signal = 'helpful'")
        .unwrap();
    let preloaded = handoff.revalidate_with_feedback(&options, &fixture.db, Some(&validated));
    assert_eq!(preloaded[&global_id].confidence, 1.0);
    assert_eq!(
        serde_json::to_value(&handoff.report.results).unwrap(),
        before
    );
    let global_hit = handoff
        .report
        .results
        .iter()
        .find(|hit| hit.doc_id == global_id)
        .unwrap();
    assert_eq!(
        global_hit.ranking_components_json().unwrap()["inputs"]["harmfulCount"],
        1
    );
    drop(validated);
    assert!(!handoff.snapshot_matches(&options, &fixture.db));
    assert!(
        run_pack_search(&options)
            .unwrap()
            .snapshot_matches(&options, &fixture.db)
    );

    // MI deduplication produces provenance on the retained global hit. It
    // must survive source rehydration alongside the calibrated intervals.
    global.memory(603, Utc::now() - chrono::Duration::days(1));
    let mut dedup_options = options.clone();
    dedup_options.query = "globalonlyquasar".to_owned();
    dedup_options.dedup_mode = SearchDedupMode::MutualInformation;
    global
        .db
        .execute_raw(
            "UPDATE memories SET content = 'globalonlyquasar release guidance preserves the authoritative source'",
        )
        .unwrap();
    let mut deduped = run_pack_search(&dedup_options).unwrap();
    assert_eq!(deduped.report.results.len(), 1);
    assert_eq!(
        deduped.report.results[0].metadata.as_ref().unwrap()["mergedFrom"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let before_dedup = serde_json::to_value(&deduped.report.results).unwrap();
    let validated = deduped
        .validate_snapshot(&dedup_options, &fixture.db)
        .unwrap();
    deduped.revalidate_with_feedback(&dedup_options, &fixture.db, Some(&validated));
    assert_eq!(
        serde_json::to_value(&deduped.report.results).unwrap(),
        before_dedup
    );
    drop(validated);
    for db in [&fixture.db, &global.db] {
        for table in ["audit_log", "pack_records", "search_index_jobs"] {
            assert_eq!(db.count_table_rows(table).unwrap(), 0);
        }
    }
}

#[test]
fn swarm_pack_reuses_global_handoff_with_canonical_pack_and_no_writes() {
    if isolate_global_store(
        "swarm_pack_reuses_global_handoff_with_canonical_pack_and_no_writes",
        "EE_TEST_GLOBAL_HANDOFF_CONTEXT_PACK",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let reference = Utc::now() + chrono::Duration::minutes(1);
    let created = reference - chrono::Duration::days(1);
    fixture.memory_with_content(
        801,
        1.0,
        1.0,
        created,
        "qualityquasar local checkout instructions use the workspace fixture.",
    );
    include_global(&fixture, true);
    let global = GlobalQualityFixture::new(
        crate::core::global_store::default_global_store_paths_from_env().unwrap(),
    );
    let global_id = global.memory(802, created);
    let _local_embedder = fixture.publish_index();
    let _global_embedder =
        crate::core::index::install_test_hash_workspace_embedder(&global.workspace);
    crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
        workspace_path: global.paths.root.clone(),
        database_path: Some(global.paths.database_path.clone()),
        index_dir: Some(global.paths.index_dir.clone()),
        dry_run: false,
    })
    .unwrap();
    let mut options = native_handoff_pack_options(&fixture, reference);
    options.memory_scope = MemoryScope::Swarm;
    options.relevance_floor = Some(0.0);
    let persistence = || {
        [&fixture.db, &global.db].map(|db| {
            ["audit_log", "pack_records", "search_index_jobs"]
                .map(|table| db.count_table_rows(table).unwrap())
        })
    };
    let before = persistence();
    let canonical =
        crate::core::context::run_context_pack_with_performance(&options, "pack").unwrap();
    let calls = AtomicUsize::new(0);
    let provider = |search: &SearchOptions, require_cached_local: bool| {
        assert!(!require_cached_local);
        calls.fetch_add(1, Ordering::SeqCst);
        let handoff = run_pack_search(search)
            .map_err(|error| SearchDegradation::daemon_fallback(&error.to_string()))?;
        PackSearchHandoff::from_value(serde_json::to_value(handoff).unwrap())
            .map_err(|error| SearchDegradation::daemon_fallback(&error.to_string()))
    };
    let remote =
        crate::core::context::run_context_pack_with_search_provider(&options, "pack", &provider)
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
    assert!(
        remote
            .response
            .data
            .pack
            .items
            .iter()
            .any(|item| item.memory_id.to_string() == global_id)
    );
    assert_eq!(persistence(), before);
}

#[test]
fn pack_handoff_binds_every_retrieval_predicate() {
    let fixture = Fixture::new();
    fixture.memory(701, 1.0, 1.0, Utc::now());
    let options = fixture.options();
    let handoff = run_pack_search(&options).unwrap();
    assert!(handoff.matches_request(&options));
    let changes: [fn(&mut SearchOptions); 13] = [
        |options| options.speed = SpeedMode::Default,
        |options| options.include_tombstoned = true,
        |options| options.include_expired = true,
        |options| options.include_future = true,
        |options| options.include_stale = true,
        |options| options.relevance_floor = Some(0.25),
        |options| options.dedup_mode = SearchDedupMode::MutualInformation,
        |options| options.explain = false,
        |options| options.memory_scope = MemoryScope::Global,
        |options| options.strict_scope = true,
        |options| options.strict_source_mode = true,
        |options| options.limit += 1,
        |options| options.index_dir = Some(options.workspace_path.join("another-index")),
    ];
    for change in changes {
        let mut changed = options.clone();
        change(&mut changed);
        assert!(!handoff.matches_request(&changed));
        assert!(!handoff.snapshot_matches(&changed, &fixture.db));
    }
    for field in ["request_identity", "global_snapshot"] {
        let mut value = serde_json::to_value(&handoff).unwrap();
        value["snapshot"].as_object_mut().unwrap().remove(field);
        let old = PackSearchHandoff::from_value(value).unwrap();
        assert!(!old.snapshot_matches(&options, &fixture.db));
    }
}

#[cfg(unix)]
#[test]
fn pack_request_identity_resolves_workspace_aliases_with_unpublished_indexes() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let alias = directory.path().join("workspace");
    std::os::unix::fs::symlink(&fixture.root, &alias).unwrap();
    let mut canonical = fixture.options();
    canonical.index_dir = Some(fixture.root.join("unpublished/nested/index"));
    let mut aliased = canonical.clone();
    aliased.workspace_path = alias.clone();
    aliased.database_path = Some(alias.join(".ee/ee.db"));
    aliased.index_dir = Some(alias.join("unpublished/nested/index"));
    assert_eq!(
        global_handoff::request_identity(&canonical),
        global_handoff::request_identity(&aliased)
    );
    assert!(!canonical.resolve_index_dir().exists());
}

#[test]
fn global_tombstones_survive_live_rebuilds_in_search_and_pack() {
    if isolate_global_store(
        "global_tombstones_survive_live_rebuilds_in_search_and_pack",
        "EE_TEST_GLOBAL_TOMBSTONE_RETRIEVAL",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let reference = Utc::now();
    let created = reference - chrono::Duration::days(1);
    fixture.memory_with_content(900, 1.0, 1.0, created, "Unrelated workspace instructions.");
    include_global(&fixture, true);
    let global = GlobalQualityFixture::new(
        crate::core::global_store::default_global_store_paths_from_env().unwrap(),
    );
    let retired = global.memory(901, created);
    let sealed = global.memory(902, created);
    let expired = global.memory(903, created);
    let held = global.memory(904, created);
    let superseded = global.memory(905, created);
    for id in [&retired, &sealed, &expired, &held, &superseded] {
        global.db.tombstone_memory(id).unwrap();
    }
    global.db.execute_raw(&format!(
        "UPDATE memories SET kind = 'failure', content = 'qualityquasar historical runbook retains prior deployment commands and evidence pointers.' WHERE id = '{retired}'"
    )).unwrap();
    global
        .db
        .set_memory_typed_fields_json(&retired, Some(r#"{"family":"historical-release"}"#))
        .unwrap();
    global
        .db
        .insert_memory_seal(
            &sealed,
            &crate::models::memory_seal_commitment(BODY.as_bytes()),
            &created.to_rfc3339(),
        )
        .unwrap();
    let expiry = (reference - chrono::Duration::hours(1)).to_rfc3339();
    global
        .db
        .execute_raw(&format!(
            "UPDATE memories SET valid_to = '{expiry}' WHERE id = '{expired}'"
        ))
        .unwrap();
    global
        .db
        .restore_imported_memory_supersession(
            &superseded,
            &(reference - chrono::Duration::minutes(1)).to_rfc3339(),
        )
        .unwrap();
    global
        .db
        .insert_feedback_quarantine(
            "fq_00000000000000000000000904",
            &crate::db::CreateFeedbackQuarantineInput {
                workspace_id: global.workspace.clone(),
                source_id: "historical-review-fixture".to_owned(),
                target_type: "memory".to_owned(),
                target_id: held.clone(),
                signal: "harmful".to_owned(),
                weight: 1.0,
                source_type: "outcome_observed".to_owned(),
                proposed_event_id: None,
                recorded_at: created.to_rfc3339(),
                reason: "Review is pending.".to_owned(),
                event_reason: None,
                evidence_json: None,
                session_id: None,
                raw_event_hash: format!("blake3:{}", "a".repeat(64)),
            },
        )
        .unwrap();
    let _local_embedder = fixture.publish_index();
    let _global_embedder =
        crate::core::index::install_test_hash_workspace_embedder(&global.workspace);
    let rebuild = crate::core::index::IndexRebuildOptions {
        workspace_path: global.paths.root.clone(),
        database_path: Some(global.paths.database_path.clone()),
        index_dir: Some(global.paths.index_dir.clone()),
        dry_run: false,
    };
    let options = SearchOptions {
        as_of: Some(reference),
        memory_scope: MemoryScope::Global,
        ..fixture.options()
    };
    let historical = SearchOptions {
        include_tombstoned: true,
        ..options.clone()
    };
    let mut expected = BTreeSet::new();
    for mixed in [false, true] {
        if mixed {
            expected.insert(global.memory(906, created));
        }
        let rebuilt = crate::core::index::rebuild_index(&rebuild).unwrap();
        assert!(rebuilt.errors.is_empty());
        assert_eq!(
            crate::core::index::index_generation_is_verified_empty(&global.paths.index_dir)
                .unwrap(),
            !mixed
        );
        let database_before = std::fs::read(&global.paths.database_path).unwrap();
        let metadata_path = global.paths.index_dir.join("meta.json");
        let metadata_before = std::fs::read(&metadata_path).unwrap();
        let default = run_search_unaudited(&options).unwrap();
        assert_eq!(
            default
                .results
                .iter()
                .map(|hit| hit.doc_id.clone())
                .collect::<BTreeSet<_>>(),
            expected
        );

        let handoff = run_pack_search(&historical).unwrap();
        assert!(handoff.can_reuse_for_pack());
        assert!(handoff.snapshot_matches(&historical, &fixture.db));
        let mut expected_historical = expected.clone();
        expected_historical.insert(retired.clone());
        for report in [run_search_unaudited(&historical).unwrap(), handoff.report] {
            assert_eq!(report.status, SearchStatus::Success);
            assert_eq!(report.results.len(), expected_historical.len());
            assert_eq!(
                report
                    .results
                    .iter()
                    .map(|hit| hit.doc_id.clone())
                    .collect::<BTreeSet<_>>(),
                expected_historical
            );
            let hit = report
                .results
                .iter()
                .find(|hit| hit.doc_id == retired)
                .unwrap();
            assert_eq!(hit.source, ScoreSource::Lexical);
            let metadata = hit.metadata.as_ref().unwrap();
            assert_eq!(
                metadata["storeLane"],
                crate::core::global_store::GLOBAL_PROVENANCE_LANE
            );
            assert_eq!(metadata["tombstoned"], true);
            assert!(metadata["tombstoned_at"].as_str().is_some());
            assert_eq!(
                metadata["provenanceUri"],
                "manual://global-feedback-quality"
            );
            let public = report.data_json();
            let retired_result = public["results"]
                .as_array()
                .unwrap()
                .iter()
                .find(|result| result["docId"] == retired)
                .unwrap();
            assert_eq!(retired_result["tombstoned"], true);
            assert!(retired_result["tombstonedAt"].is_string());
            assert!(report.degraded.iter().any(|entry| {
                entry.code == "search_tombstone_snapshot_lexical"
                    && entry.message.contains("user-global")
            }));
        }
        let filtered = run_inspection_search_with_filters(
            &historical,
            Some("failure"),
            &[TypedMemoryFieldFilter::parse("family=historical-release").unwrap()],
        )
        .unwrap();
        assert_eq!(filtered.results.len(), 1);
        assert_eq!(filtered.results[0].doc_id, retired);
        let rejected = run_inspection_search_with_filters(
            &historical,
            Some("failure"),
            &[TypedMemoryFieldFilter::parse("family=other-release").unwrap()],
        )
        .unwrap();
        assert!(rejected.results.is_empty());

        let mut pack_options = native_handoff_pack_options(&fixture, reference);
        pack_options.memory_scope = MemoryScope::Global;
        pack_options.include_tombstoned = true;
        pack_options.relevance_floor = Some(0.0);
        let pack =
            crate::core::context::run_context_pack_with_performance(&pack_options, "pack").unwrap();
        let item = pack
            .response
            .data
            .pack
            .items
            .iter()
            .find(|item| item.memory_id.to_string() == retired)
            .unwrap();
        assert!(item.tombstoned_at.is_some());
        assert!(pack.response.data.pack.items.iter().all(|item| {
            let id = item.memory_id.to_string();
            ![&sealed, &expired, &held, &superseded].contains(&&id)
        }));
        assert_eq!(
            std::fs::read(&global.paths.database_path).unwrap(),
            database_before
        );
        assert_eq!(std::fs::read(&metadata_path).unwrap(), metadata_before);
    }
    let with_expired = run_search_unaudited(&SearchOptions {
        include_expired: true,
        ..historical.clone()
    })
    .unwrap();
    assert!(with_expired.results.iter().any(|hit| hit.doc_id == expired));
    assert!(
        with_expired
            .results
            .iter()
            .all(|hit| ![&sealed, &held, &superseded].contains(&&hit.doc_id))
    );
    let before_supersession = run_search_unaudited(&SearchOptions {
        as_of: Some(reference - chrono::Duration::minutes(2)),
        ..historical.clone()
    })
    .unwrap();
    assert!(
        before_supersession
            .results
            .iter()
            .any(|hit| hit.doc_id == superseded)
    );
    assert!(
        before_supersession
            .results
            .iter()
            .all(|hit| ![&sealed, &held].contains(&&hit.doc_id))
    );

    include_global(&fixture, false);
    let excluded = run_search_unaudited(&historical).unwrap();
    assert!(excluded.results.is_empty());
    assert!(
        excluded
            .degraded
            .iter()
            .any(|entry| entry.code == "global_lane_disabled")
    );
    include_global(&fixture, true);
    std::fs::write(
        global.paths.index_dir.join("meta.json"),
        b"invalid index metadata",
    )
    .unwrap();
    let corrupt = run_search_unaudited(&historical).unwrap();
    assert!(corrupt.results.is_empty());
    assert!(
        corrupt
            .degraded
            .iter()
            .any(|entry| entry.code == "global_index_unavailable")
    );
    assert!(
        !corrupt
            .degraded
            .iter()
            .any(|entry| entry.code == "search_tombstone_snapshot_lexical")
    );
}

#[test]
fn global_tombstone_supplement_respects_modes_duplicates_and_source_budgets() {
    let fixture = Fixture::new();
    let id = fixture.memory(921, 1.0, 1.0, Utc::now() - chrono::Duration::days(1));
    fixture.db.tombstone_memory(&id).unwrap();
    let retired = fixture.db.get_memory(&id).unwrap().unwrap();
    let live_hit = SearchHit {
        doc_id: MemoryId::from_uuid(uuid::Uuid::from_u128(922)).to_string(),
        score: 0.8,
        source: ScoreSource::Lexical,
        fast_score: None,
        quality_score: None,
        lexical_score: Some(0.8),
        rerank_score: None,
        metadata: None,
        explanation: None,
    };
    let options = SearchOptions {
        speed: SpeedMode::Default,
        include_tombstoned: true,
        ..fixture.options()
    };
    let run = |options: &SearchOptions, memories: &[StoredMemory], mut hits: Vec<SearchHit>| {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let mut degraded = Vec::new();
            supplement_global_tombstone_hits(&cx, options, memories, &mut hits, &mut degraded)
                .await;
            (hits, degraded)
        })
        .unwrap()
    };
    for (source_mode, strict_source_mode, include_tombstoned) in [
        (SearchSourceMode::LexicalOnly, false, false),
        (SearchSourceMode::SemanticOnly, false, true),
        (SearchSourceMode::SemanticOnly, true, true),
        (SearchSourceMode::Hybrid, true, true),
    ] {
        let (hits, degraded) = run(
            &SearchOptions {
                source_mode,
                strict_source_mode,
                include_tombstoned,
                ..options.clone()
            },
            std::slice::from_ref(&retired),
            vec![live_hit.clone()],
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, live_hit.doc_id);
        assert!(degraded.is_empty());
    }
    for source_mode in [SearchSourceMode::LexicalOnly, SearchSourceMode::Hybrid] {
        let (hits, degraded) = run(
            &SearchOptions {
                source_mode,
                ..options.clone()
            },
            std::slice::from_ref(&retired),
            vec![live_hit.clone()],
        );
        assert_eq!(hits.len(), 2);
        assert_eq!(hits.iter().filter(|hit| hit.doc_id == id).count(), 1);
        assert!(hits.iter().all(|hit| hit.source == ScoreSource::Lexical));
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, "search_tombstone_snapshot_lexical");
        let (repeated, degraded) = run(&options, std::slice::from_ref(&retired), hits);
        assert_eq!(repeated.len(), 2);
        assert!(degraded.is_empty());
    }
    let (no_match, degraded) = run(
        &SearchOptions {
            query: "absentarchiveterm".to_owned(),
            ..options.clone()
        },
        std::slice::from_ref(&retired),
        vec![live_hit.clone()],
    );
    assert_eq!(no_match.len(), 1);
    assert!(degraded.is_empty());

    let mut oversized = retired.clone();
    oversized.content = "x".repeat(SEARCH_LIVE_SNAPSHOT_MAX_BODY_BYTES as usize + 1);
    let (hits, degraded) = run(&options, &[oversized], vec![live_hit.clone()]);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].doc_id, live_hit.doc_id);
    assert_eq!(degraded.len(), 1);
    assert_eq!(degraded[0].code, "search_tombstone_snapshot_unavailable");
    let rows = vec![retired; SEARCH_LIVE_SNAPSHOT_MAX_DOCUMENTS as usize + 1];
    let (hits, degraded) = run(&options, &rows, vec![live_hit.clone()]);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].doc_id, live_hit.doc_id);
    assert_eq!(degraded.len(), 1);
    assert_eq!(degraded[0].code, "search_tombstone_snapshot_unavailable");
}

#[test]
fn global_source_retrieval_rejects_stopword_only_matches_in_every_lifecycle() {
    if isolate_global_store(
        "global_source_retrieval_rejects_stopword_only_matches_in_every_lifecycle",
        "EE_TEST_GLOBAL_SOURCE_LEXICAL_ADMISSION",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let _local_embedder = fixture.publish_index();
    include_global(&fixture, true);
    let global = GlobalQualityFixture::new(
        crate::core::global_store::default_global_store_paths_from_env().unwrap(),
    );
    let created = Utc::now() - chrono::Duration::days(1);
    let unrelated = global.memory(931, created);
    let relevant = global.memory(932, created);
    for (id, body) in [
        (
            &unrelated,
            "How do I run release checks before shipping deployment packages?",
        ),
        (
            &relevant,
            "To bake sourdough bread, feed the starter and preheat the oven.",
        ),
    ] {
        global
            .db
            .execute(
                "UPDATE memories SET content = ?1 WHERE id = ?2",
                &[
                    sqlmodel_core::Value::Text(body.to_owned()),
                    sqlmodel_core::Value::Text(id.clone()),
                ],
            )
            .unwrap();
    }
    let _global_embedder =
        crate::core::index::install_test_hash_workspace_embedder(&global.workspace);
    let rebuild = crate::core::index::IndexRebuildOptions {
        workspace_path: global.paths.root.clone(),
        database_path: Some(global.paths.database_path.clone()),
        index_dir: Some(global.paths.index_dir.clone()),
        dry_run: false,
    };
    let mut options = SearchOptions {
        query: "how do I bake sourdough bread".to_owned(),
        relevance_floor: None,
        memory_scope: MemoryScope::Global,
        ..fixture.options()
    };
    for lifecycle in ["unpublished", "published", "retired"] {
        if lifecycle == "retired" {
            for id in [&unrelated, &relevant] {
                global.db.tombstone_memory(id).unwrap();
            }
            options.include_tombstoned = true;
        }
        if lifecycle != "unpublished" {
            let rebuilt = crate::core::index::rebuild_index(&rebuild).unwrap();
            assert!(rebuilt.errors.is_empty());
        }
        // A zero floor explicitly opts out of the overlap guard. This arm
        // proves both documents really reached the lexical engine, including
        // the negative control that matches only "how", "do" and "I".
        let control = run_pack_search(&SearchOptions {
            relevance_floor: Some(0.0),
            ..options.clone()
        })
        .unwrap();
        assert_eq!(
            control
                .report
                .results
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([unrelated.as_str(), relevant.as_str()]),
            "zero-floor control must reach both global source rows: {lifecycle}"
        );
        let guarded = run_pack_search(&options).unwrap().report;
        assert_eq!(guarded.status, SearchStatus::Success, "{lifecycle}");
        assert_eq!(guarded.results.len(), 1, "{lifecycle}");
        assert_eq!(guarded.results[0].doc_id, relevant, "{lifecycle}");
        assert!(
            guarded
                .degraded
                .iter()
                .any(|entry| entry.code == "lexical_stopword_only_filtered"),
            "stopword-only global evidence must be reported: {lifecycle}"
        );
        assert_eq!(
            guarded.data_json()["results"][0]["content_truncated"],
            false,
            "the public result contains the complete admitted source body"
        );
        if lifecycle == "unpublished" {
            assert!(
                !global.paths.index_dir.exists(),
                "read-only admission must not publish the global index"
            );
        } else {
            let direct = run_search_unaudited(&options).unwrap();
            assert_eq!(direct.results.len(), 1, "{lifecycle}");
            assert_eq!(direct.results[0].doc_id, relevant, "{lifecycle}");
        }
    }
}

#[test]
fn global_retrieval_resolves_local_ownership_before_scoring() {
    if isolate_global_store(
        "global_retrieval_resolves_local_ownership_before_scoring",
        "EE_TEST_GLOBAL_SOURCE_OWNERSHIP",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let created = Utc::now() - chrono::Duration::days(1);
    let unrelated = fixture.memory_with_content(
        941,
        1.0,
        1.0,
        created,
        "Orchard irrigation follows the weekly rainfall forecast.",
    );
    let tagged_body = "qualityquasar local incident notes preserve the cache recovery procedure.";
    let matching = fixture.memory_with_content(942, 1.0, 1.0, created, tagged_body);
    let workspace_body = "qualityquasar workspace journal records a distinct release decision.";
    let workspace_only = fixture.memory_with_content(944, 1.0, 1.0, created, workspace_body);
    let unrelated_tagged = fixture.memory_with_content(
        945,
        1.0,
        1.0,
        created,
        "Pottery glaze temperatures depend on clay and kiln cycles.",
    );
    for id in [&matching, &unrelated_tagged] {
        fixture
            .db
            .add_memory_tags(id, &["global".to_owned()])
            .unwrap();
    }
    let _local_embedder = fixture.publish_index();
    let baseline = run_pack_search(&fixture.options()).unwrap().report;
    let local_relevance = baseline
        .results
        .iter()
        .map(|hit| (hit.doc_id.clone(), hit.relevance_score()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        local_relevance.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([matching.clone(), workspace_only.clone()])
    );
    include_global(&fixture, true);
    let global = GlobalQualityFixture::new(
        crate::core::global_store::default_global_store_paths_from_env().unwrap(),
    );
    for (number, id) in [
        (941, &unrelated),
        (942, &matching),
        (944, &workspace_only),
        (945, &unrelated_tagged),
    ] {
        assert_eq!(global.memory(number, created), *id);
    }
    let global_only = global.memory(943, created);
    let _global_embedder =
        crate::core::index::install_test_hash_workspace_embedder(&global.workspace);
    let rebuild = crate::core::index::IndexRebuildOptions {
        workspace_path: global.paths.root.clone(),
        database_path: Some(global.paths.database_path.clone()),
        index_dir: Some(global.paths.index_dir.clone()),
        dry_run: false,
    };
    for lifecycle in ["unpublished", "published", "retired"] {
        if lifecycle == "retired" {
            for id in [
                &unrelated,
                &matching,
                &workspace_only,
                &unrelated_tagged,
                &global_only,
            ] {
                global.db.tombstone_memory(id).unwrap();
            }
        }
        if lifecycle != "unpublished" {
            let rebuilt = crate::core::index::rebuild_index(&rebuild).unwrap();
            assert!(rebuilt.errors.is_empty());
        }
        for scope in [
            MemoryScope::Workspace,
            MemoryScope::Swarm,
            MemoryScope::Global,
        ] {
            let options = SearchOptions {
                memory_scope: scope,
                include_tombstoned: lifecycle == "retired",
                ..fixture.options()
            };
            let handoff = run_pack_search(&options).unwrap();
            assert!(handoff.snapshot_matches(&options, &fixture.db));
            let mut reports = vec![handoff.report];
            if lifecycle != "unpublished" {
                reports.push(run_search_unaudited(&options).unwrap());
            }
            let mut expected = BTreeSet::from([matching.clone(), global_only.clone()]);
            if scope != MemoryScope::Global {
                expected.insert(workspace_only.clone());
            }
            for report in reports {
                assert_eq!(
                    report.status,
                    SearchStatus::Success,
                    "{lifecycle}, {scope:?}"
                );
                assert_eq!(
                    report
                        .results
                        .iter()
                        .map(|hit| hit.doc_id.clone())
                        .collect::<BTreeSet<_>>(),
                    expected,
                    "global relevance must not admit an unrelated local twin: {lifecycle}, {scope:?}"
                );
                for (id, body) in [(&matching, tagged_body), (&workspace_only, workspace_body)] {
                    let Some(hit) = report.results.iter().find(|hit| &hit.doc_id == id) else {
                        assert_eq!(scope, MemoryScope::Global);
                        assert_eq!(id, &workspace_only);
                        continue;
                    };
                    assert_eq!(
                        hit.relevance_score(),
                        local_relevance[id],
                        "local relevance must survive the global merge: {lifecycle}, {scope:?}"
                    );
                    let metadata = hit.metadata.as_ref().unwrap();
                    assert!(metadata.get("storeLane").is_none());
                    assert!(search_hit_content_text(metadata).unwrap().contains(body));
                }
                let distinct = report
                    .results
                    .iter()
                    .find(|hit| hit.doc_id == global_only)
                    .unwrap();
                assert_eq!(
                    distinct.metadata.as_ref().unwrap()["storeLane"],
                    crate::core::global_store::GLOBAL_PROVENANCE_LANE
                );
            }
        }
        if lifecycle == "unpublished" {
            assert!(
                !global.paths.index_dir.exists(),
                "read-only ownership admission must not publish the global index"
            );
        }
    }
}
