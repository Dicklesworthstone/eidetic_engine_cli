//! Real-store regressions for metadata-first, cursor-paged recent retrieval.

use super::*;
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
use crate::models::MemoryId;
use uuid::Uuid;

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;
const WORKSPACE: &str = "wsp_01234567890123456789012345";
const OTHER: &str = "wsp_01234567890123456789012346";
const BASE: &str = "2026-09-22T12:00:00Z";
const AT: &str = "2026-09-22T12:00:00.123456790Z";

fn id(number: u128) -> String {
    MemoryId::from_uuid(Uuid::from_u128(number)).to_string()
}

fn setup(db: &DbConnection) -> super::Result<()> {
    db.migrate()?;
    for (workspace, path) in [(WORKSPACE, "/recent-fixture"), (OTHER, "/other-fixture")] {
        db.insert_workspace(
            workspace,
            &CreateWorkspaceInput {
                path: path.to_owned(),
                name: None,
            },
        )?;
    }
    Ok(())
}

fn input() -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: WORKSPACE.to_owned(),
        level: "episodic".to_owned(),
        kind: "note".to_owned(),
        content: "Completed the release validation and queued the next review.".to_owned(),
        workflow_id: None,
        confidence: 0.8,
        utility: 0.5,
        importance: 0.5,
        provenance_uri: Some("manual://recent-fixture".to_owned()),
        trust_class: "human_explicit".to_owned(),
        trust_subclass: None,
        tags: vec!["session-recent".to_owned()],
        valid_from: None,
        valid_to: None,
    }
}

fn insert(
    db: &DbConnection,
    number: u128,
    created: &str,
    value: &CreateMemoryInput,
) -> super::Result<()> {
    db.insert_memory_with_timestamps(&id(number), value, created, created, &id(number))
}

fn identities(rows: &[StoredMemory]) -> Vec<String> {
    rows.iter().map(|memory| memory.id.clone()).collect()
}

#[test]
fn identity_scan_matches_exact_top_k_across_multiple_pages() -> TestResult {
    let db = DbConnection::open_memory()?;
    setup(&db)?;
    let count = PAGE_SIZE * 2 + 7;
    let mut expected = Vec::new();
    // Insertion order, ID order and timestamp order deliberately disagree.
    // Adjacent IDs can share an instant with different RFC3339 spellings.
    for number in (1..=count).rev() {
        let nanos = ((number * 97) % count) / 2;
        let created = if number % 2 == 0 {
            format!("2026-09-22T08:00:00.{nanos:09}-04:00")
        } else {
            format!("2026-09-22T12:00:00.{nanos:09}Z")
        };
        insert(&db, number as u128, &created, &input())?;
        expected.push((nanos, id(number as u128)));
    }
    // Independent oracle: fixture integer nanoseconds, not SQL or instant().
    expected.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    for limit in [1, 7, PAGE_SIZE - 1, PAGE_SIZE, PAGE_SIZE + 1, count] {
        let rows = db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, limit as u32)?;
        assert_eq!(
            identities(&rows),
            expected
                .iter()
                .take(limit)
                .map(|(_, id)| id.clone())
                .collect::<Vec<_>>(),
            "exact top-{limit} must not depend on the traversal page"
        );
    }
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, u32::MAX)?),
        expected.into_iter().map(|(_, id)| id).collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn expired_pages_foreign_workspaces_and_tombstones_do_not_starve_results() -> TestResult {
    let db = DbConnection::open_memory()?;
    setup(&db)?;
    let mut expired = input();
    expired.valid_to = Some(BASE.to_owned());
    for number in 1..=PAGE_SIZE * 3 {
        insert(&db, number as u128, BASE, &expired)?;
    }
    insert(&db, 1000, BASE, &input())?;
    insert(&db, 1001, "2026-09-22T12:00:00.100000000Z", &input())?;
    let mut other = input();
    other.workspace_id = OTHER.to_owned();
    insert(&db, 1002, AT, &other)?;
    insert(&db, 1003, AT, &input())?;
    db.execute_for(
        DbOperation::Execute,
        "UPDATE memories SET tombstoned_at = ?1 WHERE id = ?2",
        &[Value::Text(AT.to_owned()), Value::Text(id(1003))],
    )?;
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, 1)?),
        [id(1001)]
    );
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, u32::MAX)?),
        [id(1001), id(1000)]
    );
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(OTHER, AT, 10)?),
        [id(1002)]
    );
    Ok(())
}

#[test]
fn recognized_rfc3339_instants_do_not_depend_on_sqls_date_parser() -> TestResult {
    let db = DbConnection::open_memory()?;
    setup(&db)?;
    let stamps = [
        "2016-12-31T23:59:59.999999999Z",
        "2016-12-31T23:59:60Z",
        "2017-01-01t00:00:00z",
    ];
    for (index, stamp) in stamps.iter().enumerate() {
        assert!(
            instant(stamp).is_some(),
            "fixture must satisfy the read contract"
        );
        insert(&db, index as u128 + 1, stamp, &input())?;
    }
    let rows =
        db.list_recent_current_memories_for_retrieval(WORKSPACE, "2017-01-01T00:00:01Z", 10)?;
    assert_eq!(identities(&rows), [id(3), id(2), id(1)]);
    for (row, stamp) in rows.iter().zip(stamps.iter().rev()) {
        assert_eq!(
            row.created_at.as_str(),
            *stamp,
            "do not rewrite source timestamps"
        );
    }
    Ok(())
}

#[test]
fn ineligible_and_unselected_bodies_are_not_decoded() -> TestResult {
    let db = DbConnection::open_memory()?;
    setup(&db)?;
    let mut expired = input();
    expired.valid_to = Some(BASE.to_owned());
    insert(&db, 1, BASE, &expired)?;
    insert(&db, 2, BASE, &input())?;
    insert(&db, 3, "2026-09-22T12:00:00.100000000Z", &input())?;
    // A deliberately non-text body is a decode failure, not malformed
    // applicability metadata. Neither an expired nor a losing identity may
    // cause the selected, healthy answer to disappear during body decoding.
    for number in [1, 2] {
        db.execute_raw(&format!(
            "UPDATE memories SET content = X'80' WHERE id = '{}'",
            id(number)
        ))?;
        assert!(
            db.get_memory(&id(number)).is_err(),
            "poisoned fixture must fail decoding"
        );
    }
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, 1)?),
        [id(3)]
    );
    // Once the malformed body is actually selected it remains a hard error.
    assert!(
        db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, 2)
            .is_err()
    );
    db.begin_read_snapshot()?;
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, 1)?),
        [id(3)]
    );
    db.commit_read_snapshot()?;
    Ok(())
}

#[test]
fn selection_and_hydration_share_one_snapshot_across_a_real_writer() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("recent.db");
    let writer = DbConnection::open_file(&path)?;
    setup(&writer)?;
    for number in 1..=PAGE_SIZE + 4 {
        insert(&writer, number as u128, BASE, &input())?;
    }
    let reader = DbConnection::open_file_read_only(&path)?;
    let limit = PAGE_SIZE as u32 + 1;
    let before = reader.list_recent_current_memories_for_retrieval(WORKSPACE, AT, limit)?;
    let observed = recent_with_boundary(&reader, WORKSPACE, AT, limit, || {
        writer.execute_for(
            DbOperation::Execute,
            "UPDATE memories SET content = ?1 WHERE id = ?2",
            &[
                Value::Text("The next snapshot's replacement body.".to_owned()),
                Value::Text(id(1)),
            ],
        )?;
        writer.execute_for(
            DbOperation::Execute,
            "UPDATE memories SET tombstoned_at = ?1 WHERE id = ?2",
            &[Value::Text(AT.to_owned()), Value::Text(id(2))],
        )?;
        insert(&writer, 9000, "2026-09-22T12:00:00.100000000Z", &input())?;
        Ok(())
    })?;
    assert_eq!(
        observed, before,
        "all hydration pages must retain the selected snapshot"
    );
    let after = reader.list_recent_current_memories_for_retrieval(WORKSPACE, AT, limit)?;
    assert_eq!(
        after.first().map(|memory| memory.id.as_str()),
        Some(id(9000).as_str())
    );
    assert!(after.iter().all(|memory| memory.id != id(2)));
    assert!(after.iter().any(|memory| {
        memory.id == id(1) && memory.content == "The next snapshot's replacement body."
    }));
    reader.close()?;
    writer.close()?;
    Ok(())
}

#[test]
fn failed_selection_boundary_preserves_the_outer_read_snapshot() -> TestResult {
    let db = DbConnection::open_memory()?;
    setup(&db)?;
    insert(&db, 1, BASE, &input())?;
    db.begin_read_snapshot()?;
    let failed = recent_with_boundary(&db, WORKSPACE, AT, 1, || {
        Err(malformed("injected selection boundary failure"))
    });
    assert!(failed.is_err());
    db.commit_read_snapshot()?;
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, 1)?),
        [id(1)]
    );
    Ok(())
}

#[test]
fn failed_selection_boundary_does_not_rollback_the_callers_writes() -> TestResult {
    let db = DbConnection::open_memory()?;
    setup(&db)?;
    db.with_transaction(|| {
        insert(&db, 1, BASE, &input())?;
        assert!(
            recent_with_boundary(&db, WORKSPACE, AT, 1, || {
                Err(malformed("injected selection boundary failure"))
            })
            .is_err()
        );
        assert!(db.get_memory(&id(1))?.is_some());
        Ok(())
    })?;
    assert!(db.get_memory(&id(1))?.is_some());
    Ok(())
}

#[test]
fn zero_limit_does_not_read_storage_or_run_the_boundary() -> TestResult {
    // No schema: any accidental storage query would fail.
    let db = DbConnection::open_memory()?;
    let mut visited = false;
    let rows = recent_with_boundary(&db, WORKSPACE, AT, 0, || {
        visited = true;
        Ok(())
    })?;
    assert!(rows.is_empty());
    assert!(!visited);
    assert!(recent_with_boundary(&db, WORKSPACE, "invalid", 0, || Ok(())).is_err());
    Ok(())
}
