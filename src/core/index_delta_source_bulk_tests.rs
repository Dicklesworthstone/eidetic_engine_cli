//! Real database source admission and job publication for bulk streamed deltas.

use super::*;

#[test]
fn bulk_cass_intake_keeps_canonical_evidence_order_and_live_scope() {
    let fixture = Fixture::new(32);
    let session = insert_session(&fixture, &fixture.workspace_id);
    let revoked = insert_evidence(
        &fixture,
        &fixture.workspace_id,
        &session,
        10,
        "An earlier cited observation that is now withdrawn.",
        false,
    );
    fixture.seed_index();
    let baseline = vector_rows(&fixture.index_dir);
    assert!(baseline.contains_key(&revoked));
    let foreign = fixture.other_workspace();
    let foreign_session = insert_session(&fixture, &foreign);
    let foreign_span = insert_evidence(
        &fixture,
        &foreign,
        &foreign_session,
        9000,
        "A private observation from another workspace.",
        false,
    );
    let denied = insert_evidence(
        &fixture,
        &fixture.workspace_id,
        &session,
        9001,
        "[REDACTED:history] withheld source.",
        true,
    );
    fixture
        .db
        .execute_raw(&format!(
            "UPDATE evidence_spans SET search_eligibility = 'quarantined' WHERE id = '{revoked}'",
        ))
        .expect("withdraw current evidence admission");
    let mut added = BTreeSet::new();
    fixture.db.with_transaction(|| {
        // Reverse physical insertion order across multiple evidence pages.
        for ordinal in (2000..2513).rev() {
            added.insert(insert_evidence(
                &fixture, &fixture.workspace_id, &session, ordinal,
                &format!("Observed publication repair number {ordinal} preserves committed source evidence."),
                false,
            ));
        }
        Ok(())
    }).expect("bulk evidence transaction");
    assert_eq!(added.len(), 513);
    // A single job is not a description of the complete source mutation set.
    let job = fixture.queue(1, &memory_id(0));
    let expected = fixture.snapshot();
    let streamed = fixture.streamed().expect("bulk evidence stays streamed");
    assert_eq!(streamed.document_counts, expected.document_counts);
    assert_eq!(streamed.reconciliation.delta.removals, [revoked.clone()]);
    let order = streamed
        .reconciliation
        .delta
        .upserts
        .iter()
        .filter(|document| added.contains(&document.id))
        .map(|document| document.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(order, added.iter().cloned().collect::<Vec<_>>());
    assert_eq!(
        serde_json::to_value(streamed.reconciliation.digests).expect("streamed digests"),
        canonical_digests(&expected),
    );
    assert_delta(&fixture.process(&job));
    fixture.assert_published(&expected);
    let published = vector_rows(&fixture.index_dir);
    for absent in [&revoked, &denied, &foreign_session, &foreign_span] {
        assert!(!published.contains_key(absent), "excluded source {absent}");
    }
    for id in &added {
        assert!(published.contains_key(id));
    }
    for (id, vector) in &baseline {
        if id != &revoked {
            assert_eq!(published.get(id), Some(vector));
        }
    }
}

#[test]
fn bulk_mixed_source_is_frozen_before_late_writes_and_matches_a_full_rebuild() {
    let fixture = Fixture::new(1024);
    fixture.seed_index();
    let baseline = vector_rows(&fixture.index_dir);
    fixture.db.with_transaction(|| {
        fixture.db.execute_raw("UPDATE memories SET valid_to = '2099-12-31T00:00:00Z'")?;
        for number in 0..16 {
            fixture.db.execute_raw(&format!(
                "UPDATE memories SET content = 'Corrected source {number} records a recoverable atomic publication.' WHERE id = '{}'",
                memory_id(number),
            ))?;
        }
        for number in 960..1024 {
            fixture.db.execute_raw(&format!(
                "UPDATE memories SET tombstoned_at = '2026-10-20T00:00:00Z' WHERE id = '{}'",
                memory_id(number),
            ))?;
        }
        for number in 1024..1056 {
            insert_memory(&fixture.db, &fixture.workspace_id, &memory_id(number),
                &format!("New captured source {number} describes safe index replacement."), Vec::new())?;
        }
        Ok(())
    }).expect("mixed source transaction");
    let job = fixture.queue(1, &memory_id(0));
    let expected = fixture.snapshot();
    let streamed = fixture
        .streamed()
        .expect("large mixed source stays streamed");
    assert_eq!(streamed.reconciliation.delta.upserts.len(), 992);
    assert_eq!(streamed.reconciliation.delta.removals.len(), 64);
    assert_eq!(
        serde_json::to_value(streamed.reconciliation.digests).expect("mixed digests"),
        canonical_digests(&expected),
    );
    let reference = fixture.root.path().join("bulk-captured-reference");
    let stats = build_index_generation_sync(
        &reference,
        hash_fallback_embedder_stack(),
        expected.documents.clone(),
    )
    .expect("independent full rebuild of captured source");
    validate_built_generation(&reference, stats, expected.document_counts)
        .expect("complete reference generation");
    let captured_vectors = vector_rows(&reference);
    let late = memory_id(2000);
    let report = process_one_index_job_after_snapshot(&fixture.db, &job, &fixture.index_dir, || {
        fixture.db.execute_raw(&format!(
            "UPDATE memories SET content = 'A later mutation must not leak into the captured generation.' WHERE id = '{}'",
            memory_id(0),
        ))?;
        insert_memory(&fixture.db, &fixture.workspace_id, &late,
            "A late source belongs to the next publication, not the completed source snapshot.", Vec::new())?;
        fixture.queue(2, &late);
        Ok(())
    }).expect("publish complete captured bulk stage");
    assert_delta(&report);
    fixture.assert_published(&expected);
    assert_eq!(vector_rows(&fixture.index_dir), captured_vectors);
    for number in 16..960 {
        assert_eq!(
            captured_vectors.get(&memory_id(number)),
            baseline.get(&memory_id(number))
        );
    }
    for number in 0..16 {
        assert_ne!(
            captured_vectors.get(&memory_id(number)),
            baseline.get(&memory_id(number))
        );
    }
    let current = fixture.snapshot();
    assert!(current.generation > expected.generation);
    assert!(!captured_vectors.contains_key(&late));
    let late_job = fixture
        .db
        .get_search_index_job(&format!("sidx_{:026}", 2))
        .expect("late job lookup")
        .expect("late job");
    assert_eq!(late_job.status_enum(), Some(SearchIndexJobStatus::Pending));
    assert_delta(&fixture.process(&late_job));
    fixture.assert_published(&current);
    assert!(vector_rows(&fixture.index_dir).contains_key(&late));
}

#[test]
fn cancelling_bulk_stream_publication_restores_the_active_generation_and_allows_retry() {
    let fixture = Fixture::new(512);
    fixture.seed_index();
    let baseline = regular_files(&fixture.index_dir);
    let baseline_vectors = vector_rows(&fixture.index_dir);
    let generation = read_index_metadata(&fixture.index_dir).generation;
    fixture
        .db
        .execute_raw("UPDATE memories SET valid_to = '2099-12-31T00:00:00Z'")
        .expect("bulk metadata mutation");
    let job = fixture.queue(1, &memory_id(0));
    let job_id = job.id.clone();
    let expected = fixture.snapshot();
    let stream = fixture
        .streamed()
        .expect("bulk source must reach streaming stage");
    assert_eq!(stream.reconciliation.delta.upserts.len(), 512);
    let borrowed = &fixture;
    let cancelled = crate::core::run_cli_with_cx(Duration::from_secs(180), |cx| async move {
        install_before_index_publish_hook(|cx| {
            cx.set_cancel_reason(asupersync::CancelReason::user(
                "cancel bulk streamed publication",
            ));
        });
        process_one_index_job_with_cx(&cx, &borrowed.db, &job, &borrowed.index_dir).await
    })
    .expect("cancellation runtime");
    assert!(
        matches!(&cancelled, Err(IndexRebuildError::Cancelled(_))),
        "{cancelled:?}"
    );
    assert_eq!(regular_files(&fixture.index_dir), baseline);
    assert_eq!(
        read_index_metadata(&fixture.index_dir).generation,
        generation
    );
    assert_eq!(vector_rows(&fixture.index_dir), baseline_vectors);
    let retryable = fixture
        .db
        .get_search_index_job(&job_id)
        .expect("job lookup")
        .expect("job");
    assert_eq!(
        retryable.status_enum(),
        Some(SearchIndexJobStatus::Cancelled)
    );
    requeue_cancelled_search_index_jobs(&fixture.db, &fixture.workspace_id)
        .expect("requeue cancelled job");
    assert_delta(&fixture.process(&retryable));
    fixture.assert_published(&expected);
    assert_eq!(vector_rows(&fixture.index_dir), baseline_vectors);
}

#[test]
fn memory_pages_preserve_sidecars_history_and_unqueued_global_scope_changes() {
    let fixture = Fixture::new(385);
    let foreign = fixture.other_workspace();
    let revoked = memory_id(1000);
    let household = memory_id(1001);
    let promoted = memory_id(1002);
    for (id, tags) in [
        (&revoked, vec!["global".to_owned()]),
        (&household, vec!["house_rule".to_owned()]),
        (&promoted, Vec::new()),
    ] {
        insert_memory(
            &fixture.db,
            &foreign,
            id,
            &format!("Shared source {id} documents workspace conventions."),
            tags,
        )
        .expect("foreign source");
    }
    for (column, ordinal, stamp) in [
        ("valid_to", 127, "2020-01-01T00:00:00Z"),
        ("valid_from", 255, "2099-01-01T00:00:00Z"),
        ("superseded_at", 256, "2026-01-01T00:00:00Z"),
    ] {
        fixture
            .db
            .execute_raw(&format!(
                "UPDATE memories SET {column} = '{stamp}' WHERE id = '{}'",
                memory_id(ordinal)
            ))
            .expect("historical source fixture");
    }
    fixture.seed_index();
    let baseline = vector_rows(&fixture.index_dir);
    for id in [
        &revoked,
        &household,
        &memory_id(127),
        &memory_id(255),
        &memory_id(256),
    ] {
        assert!(baseline.contains_key(id), "index source must retain {id}");
    }
    assert!(!baseline.contains_key(&promoted));
    let before_generation = fixture
        .db
        .get_workspace_generation(&fixture.workspace_id)
        .expect("local generation");
    fixture
        .db
        .remove_memory_tags(&revoked, &["global".to_owned()])
        .expect("withdraw global");
    fixture
        .db
        .add_memory_tags(&promoted, &["global".to_owned()])
        .expect("promote global");
    assert_eq!(
        fixture
            .db
            .get_workspace_generation(&fixture.workspace_id)
            .expect("local generation"),
        before_generation,
        "the source scan must detect foreign scope edits without a local generation change"
    );
    let edges = [126, 127, 128, 255, 256, 257];
    for ordinal in edges {
        let id = memory_id(ordinal);
        let body = format!(
            "Updated page-edge source {ordinal}: validate src/page_{ordinal}.rs before committing the repair."
        );
        fixture
            .db
            .execute_raw(&format!(
                "UPDATE memories SET content = '{body}' WHERE id = '{id}'"
            ))
            .expect("page-edge content");
        assert!(
            fixture
                .db
                .refresh_memory_anchors_for_memory(&id, &body)
                .expect("source anchor extraction")
                > 0
        );
    }
    let sources_before: Vec<_> = edges
        .iter()
        .map(|ordinal| {
            let id = memory_id(*ordinal);
            (
                fixture
                    .db
                    .get_memory(&id)
                    .expect("source row")
                    .expect("row"),
                format!(
                    "{:?}",
                    fixture.db.list_memory_anchors(&id).expect("anchors")
                ),
            )
        })
        .collect();
    let job = fixture.queue(1, &memory_id(126));
    let expected = fixture.snapshot();
    let captured = fixture.streamed().expect("page-bounded source capture");
    let expected_upserts: BTreeSet<_> = edges
        .iter()
        .map(|ordinal| memory_id(*ordinal))
        .chain(std::iter::once(promoted.clone()))
        .collect();
    assert_eq!(
        captured
            .reconciliation
            .delta
            .upserts
            .iter()
            .map(|document| document.id.clone())
            .collect::<BTreeSet<_>>(),
        expected_upserts
    );
    assert_eq!(captured.reconciliation.delta.removals, [revoked.clone()]);
    assert_eq!(
        serde_json::to_value(captured.reconciliation.digests).expect("captured digests"),
        canonical_digests(&expected)
    );
    let reference = fixture.root.path().join("memory-page-reference");
    let stats = build_index_generation_sync(
        &reference,
        hash_fallback_embedder_stack(),
        expected.documents.clone(),
    )
    .expect("independent full rebuild");
    validate_built_generation(&reference, stats, expected.document_counts)
        .expect("reference completeness");
    assert_delta(&fixture.process(&job));
    fixture.assert_published(&expected);
    let published = vector_rows(&fixture.index_dir);
    assert_eq!(published, vector_rows(&reference));
    assert!(!published.contains_key(&revoked));
    assert!(published.contains_key(&promoted));
    assert_eq!(published.get(&household), baseline.get(&household));
    for (id, vector) in &baseline {
        if id != &revoked && !expected_upserts.contains(id) {
            assert_eq!(published.get(id), Some(vector));
        }
    }
    for (before, anchors) in sources_before {
        assert_eq!(
            fixture
                .db
                .get_memory(&before.id)
                .expect("unchanged source")
                .expect("source"),
            before
        );
        assert_eq!(
            format!(
                "{:?}",
                fixture
                    .db
                    .list_memory_anchors(&before.id)
                    .expect("preserved anchors")
            ),
            anchors
        );
    }
}

#[test]
fn memory_page_cancellation_stops_before_the_next_fetch_and_keeps_the_outer_snapshot() {
    let fixture = Fixture::new(257);
    fixture.db.begin_read_snapshot().expect("caller snapshot");
    let before = fixture
        .db
        .list_memories_for_retrieval_with_global(&fixture.workspace_id, None, false)
        .expect("source control");
    let borrowed = &fixture;
    let (result, pages) = crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        let mut pages = 0;
        let result = borrowed
            .db
            .visit_index_memory_batches_in_current_snapshot::<DbError>(
                &borrowed.workspace_id,
                |page| {
                    pages += 1;
                    assert_eq!(page.len(), crate::db::MEMORY_INDEX_METADATA_BATCH_SIZE);
                    cx.set_cancel_reason(asupersync::CancelReason::user(
                        "stop after the first memory page",
                    ));
                    Ok(())
                },
            );
        assert!(cx.checkpoint().is_err());
        (result, pages)
    })
    .expect("request runtime");
    assert!(result.is_err());
    assert_eq!(pages, 1);
    fixture
        .db
        .commit_read_snapshot()
        .expect("visitor must leave caller snapshot owned");
    assert_eq!(
        fixture
            .db
            .list_memories_for_retrieval_with_global(&fixture.workspace_id, None, false)
            .expect("unchanged source"),
        before
    );
}

#[test]
fn an_unreadable_later_memory_page_never_publishes_an_accepted_prefix() {
    let fixture = Fixture::new(257);
    fixture.seed_index();
    let before = regular_files(&fixture.index_dir);
    fixture.db.execute_raw(&format!("UPDATE memories SET content = 'First page contains a valid changed memory.' WHERE id = '{}'", memory_id(0)))
        .expect("early valid update");
    fixture
        .db
        .execute_raw(&format!(
            "UPDATE memories SET provenance_verified_at = X'ff' WHERE id = '{}'",
            memory_id(200)
        ))
        .expect("inject malformed second page");
    let reconciler =
        delta::SourceReconciler::read(&fixture.index_dir, &hash_fallback_embedder_stack())
            .expect("published baseline");
    let result = super::super::capture(
        &asupersync::Cx::for_testing(),
        &fixture.db,
        &fixture.workspace_id,
        reconciler,
    );
    assert!(
        result.is_err(),
        "storage corruption is not a complete or empty source snapshot"
    );
    assert_eq!(regular_files(&fixture.index_dir), before);
    fixture
        .db
        .execute_raw(&format!(
            "UPDATE memories SET provenance_verified_at = NULL WHERE id = '{}'",
            memory_id(200)
        ))
        .expect("repair fixture storage");
    let expected = fixture.snapshot();
    let job = fixture.queue(1, &memory_id(0));
    assert_delta(&fixture.process(&job));
    fixture.assert_published(&expected);
}
