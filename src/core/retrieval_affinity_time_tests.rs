// Included in the existing real-store affinity test module so its fixtures,
// canonical IDs, and regression-workflow selection stay unchanged.

const TIME_OLD: &str = "2026-07-01T00:00:00Z";
const TIME_NEW: &str = "2026-07-31T00:00:00Z";

fn time_instant(value: &str) -> DateTime<Utc> {
    parse_rfc3339(value).expect("fixture instant")
}

fn time_metrics(connection: &DbConnection, workspace: &str) -> serde_json::Value {
    let snapshot = connection
        .get_latest_graph_snapshot(workspace, GraphSnapshotType::RetrievalAffinity)
        .expect("latest snapshot")
        .expect("persisted snapshot");
    serde_json::from_str(&snapshot.metrics_json).expect("snapshot metrics")
}

fn time_snapshot_hash(connection: &DbConnection, workspace: &str) -> String {
    let materialized =
        materialize_retrieval_affinity_snapshot(connection, workspace, 1, 30.0)
            .expect("materialize");
    match materialized {
        AffinityMaterialization::Persisted { content_hash, .. } => content_hash,
        AffinityMaterialization::Cold => panic!("fixture has an edge"),
    }
}

#[test]
fn time_newer_singleton_does_not_rejuvenate_an_unrelated_pair() {
    let (_temp, connection, workspace) = seeded_connection();
    seed_search_set(&connection, &workspace, "time_pair", &ATOMIC_HITS[..2], TIME_OLD);
    seed_search_set(&connection, &workspace, "time_solo", &ATOMIC_HITS[2..], TIME_NEW);
    let report = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW)
        .expect("consume pair and singleton");
    assert_eq!((report.search_rows_consumed, report.pairs_updated), (3, 1));
    let edges = connection.list_retrieval_affinity_edges(&workspace).expect("edges");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].2, 0.5);
    assert_eq!(edges[0].3, "2026-07-01T00:00:00.000000000Z");
    time_snapshot_hash(&connection, &workspace);
    assert_eq!(time_metrics(&connection, &workspace)["asOf"], edges[0].3);
}

#[test]
fn time_distinct_pairs_in_one_batch_decay_from_their_own_evidence() {
    let (_temp, connection, workspace) = seeded_connection();
    let newer = [stream_id(4), stream_id(5)];
    seed_search_set(&connection, &workspace, "time_old", &ATOMIC_HITS[..2], TIME_OLD);
    seed_search_set(
        &connection,
        &workspace,
        "time_new",
        &[(newer[0].as_str(), 1), (newer[1].as_str(), 2)],
        TIME_NEW,
    );
    accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).expect("consume");
    time_snapshot_hash(&connection, &workspace);
    let metrics = time_metrics(&connection, &workspace);
    assert_eq!(metrics["asOf"], "2026-07-31T00:00:00.000000000Z");
    assert_eq!(metrics["edges"][0]["weight"], 0.25);
    assert_eq!(metrics["edges"][1]["weight"], 0.5);
}

#[test]
fn time_pack_pair_expansion_keeps_pair_specific_instants_and_the_rank_cap() {
    let old = (1_u32..=34)
        .map(|rank| (stream_id(u128::from(rank)), rank))
        .collect::<Vec<_>>();
    let newer = vec![(stream_id(100), 1), (stream_id(101), 4)];
    let mut deltas = AffinityDeltas::new();
    assert_eq!(accumulate_pairs(&mut deltas, &old, time_instant(TIME_OLD)), 496);
    assert_eq!(accumulate_pairs(&mut deltas, &newer, time_instant(TIME_NEW)), 1);
    assert_eq!(deltas.len(), 497);
    let old_edge = &deltas[&(stream_id(1), stream_id(2))];
    assert_eq!((old_edge.weight, old_edge.last_event_at), (0.5, time_instant(TIME_OLD)));
    let new_edge = &deltas[&(stream_id(100), stream_id(101))];
    assert_eq!((new_edge.weight, new_edge.last_event_at), (0.25, time_instant(TIME_NEW)));
    assert!(!deltas.keys().any(|(a, b)| a == &stream_id(34) || b == &stream_id(34)));
}

#[test]
fn time_cross_page_pair_retains_a_newer_timestamp_from_the_replayed_prefix() {
    let (_temp, connection, workspace) = seeded_connection();
    connection.with_transaction(|| {
        for index in 0..ACCUMULATION_BATCH_LIMIT - 1 {
            seed_search_set(
                &connection,
                &workspace,
                &format!("time_pad{index}"),
                &ATOMIC_HITS[..1],
                TIME_OLD,
            );
        }
        seed_search_set(&connection, &workspace, "time_split", &ATOMIC_HITS[..1], TIME_NEW);
        Ok(())
    }).expect("seed first page");
    let left = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW)
        .expect("first page");
    assert!(left.more_pending);
    assert_eq!(left.pairs_updated, 0);
    // Use the same retained query hash but a distinct audit id. The earlier
    // audit position has the later instant; page-local time would lose it.
    connection.execute_raw(&format!(
        "INSERT INTO audit_log (id, workspace_id, timestamp, action, target_type, target_id, details) VALUES ('{}', '{workspace}', '{TIME_OLD}', 'search.returned_mem', 'memory', '{}', '{{\"queryHash\":\"time_split\",\"rank\":2}}')",
        crate::db::generate_audit_id(),
        ATOMIC_HITS[1].0,
    )).expect("append continuation");
    let right = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW)
        .expect("second page");
    assert_eq!((right.search_rows_consumed, right.pairs_updated), (1, 1));
    let edges = connection.list_retrieval_affinity_edges(&workspace).expect("edges");
    assert_eq!(edges[0].3, "2026-07-31T00:00:00.000000000Z");
    assert_eq!(edges[0].2, 0.5);
    assert_eq!(accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW)
        .expect("replay").pairs_updated, 0);
}

#[test]
fn time_out_of_order_batch_after_reopen_does_not_move_an_edge_backwards() {
    let (temp, connection, workspace) = seeded_connection();
    seed_search_set(&connection, &workspace, "time_first", &ATOMIC_HITS[..2], TIME_NEW);
    accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).expect("newer prefix");
    connection.close().expect("close");
    let connection = DbConnection::open_file(&temp.path().join("ee.db")).expect("reopen");
    seed_search_set(&connection, &workspace, "time_later", &ATOMIC_HITS[..2], TIME_OLD);
    accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).expect("older event");
    let edges = connection.list_retrieval_affinity_edges(&workspace).expect("edges");
    assert_eq!(edges[0].2, 1.0);
    assert_eq!(edges[0].3, "2026-07-31T00:00:00.000000000Z");
}

#[test]
fn time_legacy_offsets_and_optional_fractions_are_normalized_when_touched() {
    for (stored, incoming, expected) in [
        ("2026-08-02T01:00:00+02:00", "2026-08-02T00:30:00Z", "2026-08-02T00:30:00.000000000Z"),
        ("2026-08-02T00:00:00Z", "2026-08-02T00:00:00.1Z", "2026-08-02T00:00:00.100000000Z"),
        ("2026-08-02T00:00:00-02:00", "2026-08-02T01:00:00Z", "2026-08-02T02:00:00.000000000Z"),
    ] {
        let (_temp, connection, workspace) = seeded_connection();
        connection.with_transaction(|| {
            connection.apply_retrieval_affinity_deltas(
                &workspace,
                &[(ATOMIC_HITS[0].0.to_owned(), ATOMIC_HITS[1].0.to_owned(), 0.5)],
                stored,
            )
        }).expect("legacy row");
        seed_search_set(&connection, &workspace, "time_touch", &ATOMIC_HITS[..2], incoming);
        accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).expect("normalize");
        let edges = connection.list_retrieval_affinity_edges(&workspace).expect("edges");
        assert_eq!(edges[0].2, 1.0);
        assert_eq!(edges[0].3, expected);
    }
}

#[test]
fn time_equivalent_timestamp_spellings_produce_the_same_snapshot_hash() {
    let (_temp, connection, workspace) = seeded_connection();
    seed_search_set(&connection, &workspace, "time_equal", &ATOMIC_HITS[..2], TIME_OLD);
    accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).expect("consume");
    let expected = time_snapshot_hash(&connection, &workspace);
    for spelling in ["2026-07-01T02:00:00+02:00", "2026-06-30T20:00:00-04:00", "2026-07-01T00:00:00.000Z"] {
        connection.execute_raw(&format!(
            "UPDATE retrieval_affinity_accumulation SET last_event_at = '{spelling}' WHERE workspace_id = '{workspace}'"
        )).expect("legacy spelling");
        assert_eq!(time_snapshot_hash(&connection, &workspace), expected);
    }
}

#[test]
fn time_materialization_uses_chronological_not_lexical_maximum() {
    let (_temp, connection, workspace) = seeded_connection();
    connection.with_transaction(|| {
        connection.apply_retrieval_affinity_deltas(
            &workspace,
            &[(stream_id(1), stream_id(2), 0.5)],
            "2026-08-02T01:00:00+02:00",
        )?;
        connection.apply_retrieval_affinity_deltas(
            &workspace,
            &[(stream_id(4), stream_id(5), 0.5)],
            "2026-08-02T00:30:00Z",
        )
    }).expect("legacy timestamps");
    // Ninety minutes is exactly the gap between these two UTC instants.
    materialize_retrieval_affinity_snapshot(&connection, &workspace, 1, 90.0 / 1440.0)
        .expect("materialize legacy instants");
    let metrics = time_metrics(&connection, &workspace);
    assert_eq!(metrics["asOf"], "2026-08-02T00:30:00.000000000Z");
    assert_eq!(metrics["edges"][0]["weight"], 0.25);
    assert_eq!(metrics["edges"][1]["weight"], 0.5);
}

#[test]
fn time_normalization_failure_rolls_back_weight_and_cursor_then_retries_once() {
    let (_temp, connection, workspace) = seeded_connection();
    seed_search_set(&connection, &workspace, "time_fault", &ATOMIC_HITS[..2], TIME_OLD);
    connection.execute_raw(
        "CREATE TRIGGER time_fail_normalize BEFORE UPDATE ON retrieval_affinity_accumulation WHEN NEW.weight = OLD.weight BEGIN SELECT RAISE(ABORT, 'private-time-fault'); END;"
    ).expect("fault after the additive write");
    let error = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW)
        .expect_err("normalization must be atomic with the weight");
    assert!(!error.contains("private-time-fault"));
    assert!(connection.list_retrieval_affinity_edges(&workspace).expect("rollback").is_empty());
    assert_eq!(connection.retrieval_affinity_cursor(&workspace).expect("cursor"), (0, 0));
    connection.execute_raw("DROP TRIGGER time_fail_normalize").expect("remove fault");
    accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).expect("retry");
    let edges = connection.list_retrieval_affinity_edges(&workspace).expect("once counted");
    assert_eq!(edges[0].2, 0.5);
    assert_eq!(accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW)
        .expect("repeat").pairs_updated, 0);
}

#[test]
fn time_corrupt_stored_timestamp_withholds_the_new_prefix_and_snapshot() {
    let (_temp, connection, workspace) = seeded_connection();
    seed_search_set(&connection, &workspace, "time_good", &ATOMIC_HITS[..2], TIME_OLD);
    accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).expect("valid prefix");
    let cursor = connection.retrieval_affinity_cursor(&workspace).expect("cursor");
    connection.execute_raw(
        "UPDATE retrieval_affinity_accumulation SET last_event_at = 'private-time-canary'"
    ).expect("corrupt retained metadata");
    seed_search_set(&connection, &workspace, "time_next", &ATOMIC_HITS[..2], TIME_NEW);
    let error = accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW)
        .expect_err("withhold corrupt prefix");
    assert!(!error.contains("private-time-canary"));
    assert_eq!(connection.retrieval_affinity_cursor(&workspace).expect("unchanged cursor"), cursor);
    assert_eq!(connection.list_retrieval_affinity_edges(&workspace).expect("weight")[0].2, 0.5);
    let error = materialize_retrieval_affinity_snapshot(&connection, &workspace, 1, 30.0)
        .expect_err("a corrupt timestamp is not fresh evidence");
    assert!(!error.contains("private-time-canary"));
    assert!(connection.get_latest_graph_snapshot(&workspace, GraphSnapshotType::RetrievalAffinity)
        .expect("no snapshot").is_none());
}

#[test]
fn time_nonfinite_half_lives_cannot_publish_snapshot_metadata() {
    let (_temp, connection, workspace) = seeded_connection();
    seed_search_set(&connection, &workspace, "time_finite", &ATOMIC_HITS[..2], TIME_OLD);
    accumulate_retrieval_affinity(&connection, &workspace, ATOMIC_NOW).expect("consume");
    for half_life in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(materialize_retrieval_affinity_snapshot(&connection, &workspace, 1, half_life).is_err());
    }
    assert!(connection.get_latest_graph_snapshot(&workspace, GraphSnapshotType::RetrievalAffinity)
        .expect("no invalid snapshots").is_none());
    // Keep the existing finite nonpositive default behavior.
    assert!(matches!(materialize_retrieval_affinity_snapshot(&connection, &workspace, 1, 0.0)
        .expect("default half-life"), AffinityMaterialization::Persisted { .. }));
}

#[test]
fn time_persisted_dates_and_decay_match_across_different_processing_partitions() {
    let (_temp, connection, workspace) = seeded_connection();
    let targets = [stream_id(1), stream_id(2), stream_id(3), stream_id(4)];
    for (index, timestamp) in [TIME_NEW, TIME_OLD, "2026-07-15T01:00:00+02:00", TIME_NEW]
        .into_iter().enumerate()
    {
        let hits = if index % 2 == 0 { &targets[..2] } else { &targets[2..] };
        seed_search_set(&connection, &workspace, &format!("time_part{index}"),
            &[(hits[0].as_str(), 1), (hits[1].as_str(), 2)], timestamp);
    }
    let mut expected = None;
    for limit in [1, 2, 3, 7, 512] {
        connection.execute_raw("DELETE FROM retrieval_affinity_accumulation").expect("reset derived test projection");
        let mut cursor = 0;
        loop {
            let page = connection.with_transaction(|| {
                let mut deltas = AffinityDeltas::new();
                let page = accumulate_search_page(&connection, &workspace, cursor, limit, &mut deltas)?;
                let rows = deltas.into_iter().map(|((a, b), delta)|
                    (a, b, delta.weight, delta.last_event_at)).collect::<Vec<_>>();
                connection.apply_retrieval_affinity_timed_deltas(&workspace, &rows)?;
                Ok(page)
            }).expect("persist page");
            cursor = page.cursor;
            if page.raw_rows < limit as usize { break; }
        }
        let edges = connection.list_retrieval_affinity_edges(&workspace).expect("edges");
        let hash = time_snapshot_hash(&connection, &workspace);
        if let Some((expected_edges, expected_hash)) = &expected {
            assert_eq!(&edges, expected_edges, "partition {limit}");
            assert_eq!(&hash, expected_hash, "partition {limit}");
        } else {
            expected = Some((edges, hash));
        }
    }
}
