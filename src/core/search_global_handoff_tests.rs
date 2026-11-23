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
