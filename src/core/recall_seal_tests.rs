// Included in scan::tests: exercise the production metadata query, admission,
// candidate cap, hydration and public rendering against real FrankenSQLite.

const SEALED_RECALL_BODY: &str =
    "SEALED-RECALL-CANARY anchor:path:src/release.rs anchor:symbol:Release::publish";
const PUBLIC_RECALL_BODY: &str =
    "Public release guidance. anchor:path:src/release.rs anchor:symbol:Release::publish";

fn seal_queries() -> [RecallQuery; 3] {
    [
        RecallQuery {
            paths: vec!["src/*".to_owned()],
            ..RecallQuery::default()
        },
        RecallQuery {
            symbols: vec!["Release::publish".to_owned()],
            ..RecallQuery::default()
        },
        RecallQuery {
            diff_paths: vec!["src/release.rs".to_owned()],
            ..RecallQuery::default()
        },
    ]
}

fn seal_commitment() -> String {
    crate::models::memory_seal_commitment(SEALED_RECALL_BODY.as_bytes())
}

// Only the temporary test store loses its write-time constraints. This models
// legacy/corrupt durable metadata without weakening production migrations or
// replacing the database, source reader or canonical model validator.
fn allow_legacy_seal_rows(db: &DbConnection) {
    db.execute_raw("ALTER TABLE memory_seals RENAME TO fixture_original_memory_seals")
        .unwrap();
    db.execute_raw(
        "CREATE TABLE memory_seals (memory_id TEXT PRIMARY KEY, content_commitment, sealed_at, revealed_at, reveal_verified)",
    )
    .unwrap();
}

fn seed_revealed_seal(db: &DbConnection, id: &str) {
    db.execute_raw(&format!(
        "INSERT INTO memory_seals (memory_id, content_commitment, sealed_at, revealed_at, reveal_verified) VALUES ('{id}', '{}', '{BASE}', '{BASE}', 1)",
        seal_commitment(),
    ))
    .unwrap();
}

fn reset_seal(db: &DbConnection, id: &str) {
    db.execute_raw(&format!(
        "UPDATE memory_seals SET content_commitment = '{}', sealed_at = '{BASE}', revealed_at = '{BASE}', reveal_verified = 1 WHERE memory_id = '{id}'",
        seal_commitment(),
    ))
    .unwrap();
}

#[test]
fn seal_reveal_requires_verification_for_every_recall_selector() {
    let db = fixture();
    let hidden = seed(&db, 1, SEALED_RECALL_BODY, 0.99);
    let public = seed(&db, 2, PUBLIC_RECALL_BODY, 0.1);
    allow_legacy_seal_rows(&db);
    seed_revealed_seal(&db, &hidden);
    for verified in ["NULL", "0"] {
        db.execute_raw(&format!(
            "UPDATE memory_seals SET reveal_verified = {verified}"
        ))
        .unwrap();
        for query in seal_queries() {
            // A hidden high scorer must not consume the single retained slot.
            let result = scan(&db, &query, 100, 1);
            assert_eq!(result.rows.len(), 1);
            assert_eq!(result.rows[0].memory_id, public);
            assert!(result.degraded.iter().any(|d| {
                d.code == "recall_source_filtered"
                    && d.severity == "medium"
                    && d.message.contains("malformed=1")
            }));
            assert!(!format!("{:?}", result.rows).contains(&hidden));
        }
    }
    reset_seal(&db, &hidden);
    for query in seal_queries() {
        let result = scan(&db, &query, 100, 1);
        assert_eq!(result.rows[0].memory_id, hidden);
        assert_eq!(result.rows[0].content, SEALED_RECALL_BODY);
    }
}

#[test]
fn seal_invalid_fields_never_become_revealed_advice() {
    let db = fixture();
    let hidden = seed(&db, 1, SEALED_RECALL_BODY, 0.9);
    allow_legacy_seal_rows(&db);
    seed_revealed_seal(&db, &hidden);
    for assignment in [
        "content_commitment = 'PRIVATE-BROKEN-COMMITMENT'",
        "content_commitment = NULL",
        "content_commitment = 7",
        "sealed_at = 'PRIVATE-BROKEN-SEALED-TIME'",
        "sealed_at = NULL",
        "sealed_at = 7",
        "revealed_at = 'PRIVATE-BROKEN-REVEAL-TIME'",
        "revealed_at = 7",
        "revealed_at = '2025-12-31T23:59:59.999999999Z'",
        "revealed_at = NULL, reveal_verified = 1",
        "revealed_at = NULL, reveal_verified = 0",
        "reveal_verified = 2",
        "reveal_verified = -1",
        "reveal_verified = '1'",
        "reveal_verified = 1.5",
    ] {
        reset_seal(&db, &hidden);
        db.execute_raw(&format!("UPDATE memory_seals SET {assignment}"))
            .unwrap();
        let result = scan(&db, &seal_queries()[0], 100, 10);
        assert!(result.rows.is_empty(), "{assignment}");
        assert!(result.degraded.iter().any(|d| {
            d.code == "recall_source_filtered" && d.message.contains("malformed=1")
        }));
        let output = format!("{:?}", result.degraded);
        assert!(!output.contains("PRIVATE-BROKEN"));
        assert!(!output.contains(&hidden));
        assert!(!output.contains("SEALED-RECALL-CANARY"));
    }
}

#[test]
fn seal_verified_reveal_uses_exact_instant_order_not_text_order() {
    let db = fixture();
    let id = seed(&db, 1, SEALED_RECALL_BODY, 0.9);
    allow_legacy_seal_rows(&db);
    seed_revealed_seal(&db, &id);
    for (sealed, revealed, visible) in [
        ("2026-01-01T02:00:00+02:00", BASE, true),
        ("2026-01-01T00:00:00-02:00", "2026-01-01T01:00:00Z", false),
        ("2026-01-01T00:00:00.1Z", "2026-01-01T00:00:00.100000000Z", true),
        ("2026-01-01T00:00:00.1Z", "2026-01-01T00:00:00.100000001Z", true),
        ("2026-01-01T00:00:00.1Z", "2026-01-01T00:00:00.099999999Z", false),
    ] {
        db.execute_raw(&format!(
            "UPDATE memory_seals SET sealed_at = '{sealed}', revealed_at = '{revealed}'"
        ))
        .unwrap();
        assert_eq!(
            !scan(&db, &seal_queries()[0], 100, 10).rows.is_empty(),
            visible,
            "{sealed} -> {revealed}",
        );
    }
}

#[test]
fn seal_closed_plaintext_and_invalid_reveals_are_absent_from_public_output() {
    let db = fixture();
    let hidden = seed(&db, 1, SEALED_RECALL_BODY, 0.9);
    allow_legacy_seal_rows(&db);
    seed_revealed_seal(&db, &hidden);
    for (assignment, reason) in [
        ("revealed_at = NULL, reveal_verified = NULL", "sealed=1"),
        ("reveal_verified = 0", "malformed=1"),
    ] {
        reset_seal(&db, &hidden);
        db.execute_raw(&format!("UPDATE memory_seals SET {assignment}"))
            .unwrap();
        for query in seal_queries() {
            let report = super::super::run_recall(&db, WORKSPACE, &query).unwrap();
            assert!(report.items.is_empty());
            assert_eq!(report.total_matched, 0);
            assert!(!report.truncated);
            assert!(report.continuation_cursor.is_none());
            assert!(report.degraded.iter().any(|d| d.message.contains(reason)));
            for output in [
                super::super::recall_data_json(
                    &report,
                    &super::super::RecallQueryEcho::default(),
                )
                .to_string(),
                super::super::render_recall_markdown(&report, &[]),
                format!("{report:?}"),
            ] {
                assert!(!output.contains(&hidden));
                assert!(!output.contains("SEALED-RECALL-CANARY"));
                assert!(!output.contains(&seal_commitment()));
            }
        }
        // The source stayed intact: withholding is not a silent rewrite.
        assert_eq!(db.get_memory(&hidden).unwrap().unwrap().content, SEALED_RECALL_BODY);
    }
}

#[test]
fn seal_invalid_pages_do_not_starve_a_later_public_source() {
    let db = fixture();
    allow_legacy_seal_rows(&db);
    db.with_transaction(|| {
        for number in 1..=PAGE_SIZE + 2 {
            let id = seed(&db, u32::try_from(number).unwrap(), SEALED_RECALL_BODY, 0.99);
            seed_revealed_seal(&db, &id);
        }
        db.execute_raw("UPDATE memory_seals SET reveal_verified = 0")?;
        seed(&db, 9999, PUBLIC_RECALL_BODY, 0.1);
        Ok(())
    })
    .unwrap();
    let result = scan(&db, &seal_queries()[0], 1024, 1);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].memory_id, format!("mem_{:026}", 9999));
    assert!(result.degraded.iter().any(|d| d.message.contains("malformed=258")));
    assert!(!result.degraded.iter().any(|d| d.code == "recall_scan_incomplete"));
}

#[test]
fn seal_missing_authority_column_cannot_return_a_partial_report() {
    let db = fixture();
    seed(&db, 1, PUBLIC_RECALL_BODY, 0.9);
    db.execute_raw("ALTER TABLE memory_seals RENAME COLUMN reveal_verified TO unavailable_verification")
        .unwrap();
    let error = super::super::run_recall(&db, WORKSPACE, &seal_queries()[0]).unwrap_err();
    assert!(error.to_string().contains("no partial result"));
    assert!(!error.to_string().contains(PUBLIC_RECALL_BODY));
    db.begin_read_snapshot().expect("owned recall snapshot released on failure");
    db.rollback_read_snapshot().unwrap();
}

#[test]
fn seal_authority_and_bodies_stay_in_the_same_read_only_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("recall-seals.db");
    let writer = DbConnection::open_file(&path).unwrap();
    writer.migrate().unwrap();
    writer.insert_workspace(
        WORKSPACE,
        &CreateWorkspaceInput {
            path: root.path().to_string_lossy().into_owned(),
            name: None,
        },
    ).unwrap();
    let id = seed(&writer, 1, SEALED_RECALL_BODY, 0.9);
    allow_legacy_seal_rows(&writer);
    seed_revealed_seal(&writer, &id);
    let reader = DbConnection::open_file_read_only(&path).unwrap();
    let query = &seal_queries()[0];
    let at = DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
        .unwrap().with_timezone(&Utc);
    let snapshot = super::super::RecallReadSnapshot::begin_db(&reader).unwrap();
    let before = super::super::run_recall_in_snapshot(&reader, WORKSPACE, query, at).unwrap();
    assert_eq!(before.items.len(), 1);
    writer.execute_raw("UPDATE memory_seals SET reveal_verified = 0").unwrap();
    let pinned = super::super::run_recall_in_snapshot(&reader, WORKSPACE, query, at).unwrap();
    assert_eq!(pinned.items, before.items);
    snapshot.finish_db().unwrap();
    let body = writer.get_memory(&id).unwrap();
    let audits = writer.count_table_rows("audit_log").unwrap();
    let next = super::super::run_recall(&reader, WORKSPACE, query).unwrap();
    assert!(next.items.is_empty());
    assert!(next.degraded.iter().any(|d| d.message.contains("malformed=1")));
    assert_eq!(writer.get_memory(&id).unwrap(), body);
    assert_eq!(writer.count_table_rows("audit_log").unwrap(), audits);
}

#[test]
fn seal_absent_join_and_incomplete_projection_are_distinct() {
    let db = fixture();
    let id = seed(&db, 1, PUBLIC_RECALL_BODY, 0.9);
    let at = DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
        .unwrap().with_timezone(&Utc);
    let mut cells = vec![Value::Null; 25];
    cells[0] = Value::Text(id.clone());
    cells[1] = Value::Text(WORKSPACE.to_owned());
    cells[2] = Value::Text(BASE.to_owned());
    cells[3] = Value::Text(BASE.to_owned());
    let select = (1..=25).map(|n| format!("?{n}")).collect::<Vec<_>>().join(", ");
    let query = format!("SELECT {select}");
    let admit = |cells: &[Value]| {
        let rows = db.query(&query, cells).unwrap();
        super::super::admission::denial(&rows[0], &id, WORKSPACE, at)
    };
    assert_eq!(admit(&cells), None, "all five absent seal cells");
    cells[22] = Value::Text(seal_commitment());
    assert_eq!(admit(&cells), Some("malformed"), "an inconsistent absent join");
    cells[8] = Value::Text("not-the-source-memory".to_owned());
    cells[9] = Value::Text(BASE.to_owned());
    cells[23] = Value::Text(BASE.to_owned());
    cells[24] = Value::BigInt(1);
    assert_eq!(admit(&cells), Some("malformed"), "wrong seal owner");
    cells[8] = Value::Text(id.clone());
    assert_eq!(admit(&cells), None, "complete verified seal");
    let short = (1..=22).map(|n| format!("?{n}")).collect::<Vec<_>>().join(", ");
    let rows = db.query(&format!("SELECT {short}"), &cells[..22]).unwrap();
    assert_eq!(
        super::super::admission::denial(&rows[0], &id, WORKSPACE, at),
        Some("malformed"),
        "omitted authority columns cannot masquerade as an absent seal",
    );
}
