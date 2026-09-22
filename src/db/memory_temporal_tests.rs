use super::*;
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
use crate::models::MemoryId;
use uuid::Uuid;

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;
const WORKSPACE: &str = "wsp_01234567890123456789012345";
const BASE: &str = "2026-09-22T12:00:00Z";
const AT: &str = "2026-09-22T12:00:00.123456790Z";

fn setup(db: &DbConnection) -> super::Result<()> {
    db.migrate()?;
    db.insert_workspace(
        WORKSPACE,
        &CreateWorkspaceInput {
            path: "/temporal-fixture".to_owned(),
            name: None,
        },
    )?;
    Ok(())
}

fn id(number: u128) -> String {
    MemoryId::from_uuid(Uuid::from_u128(number)).to_string()
}

fn input() -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: WORKSPACE.to_owned(),
        level: "semantic".to_owned(),
        kind: "fact".to_owned(),
        content: "Preserve the exact release applicability window.".to_owned(),
        workflow_id: None,
        confidence: 0.8,
        utility: 0.5,
        importance: 0.5,
        provenance_uri: Some("manual://temporal-fixture".to_owned()),
        trust_class: "human_explicit".to_owned(),
        trust_subclass: None,
        tags: vec!["release".to_owned()],
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
    rows.iter().map(|row| row.id.clone()).collect()
}

#[test]
fn applicability_and_tag_surfaces_use_exact_inclusive_expiry() -> TestResult {
    let db = DbConnection::open_memory()?;
    setup(&db)?;
    for (number, end) in [
        (1, "2026-09-22T12:00:00.123456789Z"),
        (2, "2026-09-22T08:00:00.123456790-04:00"),
        (3, "2026-09-22T12:00:00.123456791Z"),
        (4, "2026-09-22T12:00:00Z"),
        (5, "2026-09-22T12:00:01+00:00"),
        (6, "not-a-timestamp"),
    ] {
        let mut value = input();
        value.valid_to = Some(end.to_owned());
        value.tags.push(format!("expiry-{number}"));
        insert(&db, number, BASE, &value)?;
    }
    let expected = vec![id(2), id(3), id(5)];
    assert_eq!(
        identities(&db.list_memories_valid_at(WORKSPACE, None, false, AT)?),
        expected
    );
    assert_eq!(
        db.list_memories_by_tag_valid_at(WORKSPACE, "release", AT)?,
        expected
    );
    assert_eq!(
        db.list_all_tags_valid_at(WORKSPACE, AT)?,
        ["expiry-2", "expiry-3", "expiry-5", "release"]
    );
    assert_eq!(
        db.get_tag_counts_valid_at(WORKSPACE, AT)?,
        vec![
            TagCount {
                tag: "release".into(),
                count: 3
            },
            TagCount {
                tag: "expiry-2".into(),
                count: 1
            },
            TagCount {
                tag: "expiry-3".into(),
                count: 1
            },
            TagCount {
                tag: "expiry-5".into(),
                count: 1
            },
        ]
    );
    assert_eq!(
        db.list_memories_valid_at(WORKSPACE, None, true, AT)?.len(),
        6
    );
    assert!(
        db.list_memories_valid_at(WORKSPACE, Some("procedural"), false, AT)?
            .is_empty()
    );
    assert_eq!(
        identities(&db.list_memories_valid_at(
            WORKSPACE,
            None,
            false,
            "2026-09-22T13:00:00.123456790+01:00"
        )?),
        expected
    );
    Ok(())
}

#[test]
fn recency_applies_exact_eligibility_before_limit_and_finishes_coarse_ties() -> TestResult {
    let db = DbConnection::open_memory()?;
    setup(&db)?;
    // More than a page of future records share the same rounded Julian day.
    // A SQL LIMIT 1 can hide the actual answer entirely.
    for number in 1..=PAGE_SIZE as u128 + 2 {
        insert(&db, number, "2026-09-22T12:00:00.123456791Z", &input())?;
    }
    insert(&db, 500, "2026-09-22T12:00:00.123456780Z", &input())?;
    insert(&db, 501, "2026-09-22T12:00:00.123456789Z", &input())?;
    insert(&db, 502, "2026-09-22T13:00:00.123456789+01:00", &input())?;
    insert(&db, 503, "2026-09-22T12:00:00.123456788+00:00", &input())?;
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, 1)?),
        [id(501)]
    );
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, 3)?),
        [id(501), id(502), id(503)]
    );
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(
            WORKSPACE,
            "2026-09-22T08:00:00.123456790-04:00",
            4
        )?),
        [id(501), id(502), id(503), id(500)]
    );
    assert!(
        db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, 0)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn recency_distinguishes_one_nanosecond_lifecycle_boundaries() -> TestResult {
    let db = DbConnection::open_memory()?;
    setup(&db)?;
    for number in 1..=8 {
        insert(&db, number, BASE, &input())?;
    }
    for (number, column, value) in [
        (1, "valid_from", "2026-09-22T12:00:00.123456791Z"),
        (2, "valid_to", "2026-09-22T12:00:00.123456789Z"),
        (3, "superseded_at", AT),
        (4, "updated_at", "2026-09-22T12:00:00.123456791Z"),
        (5, "valid_to", AT),
        (6, "valid_from", AT),
        (7, "superseded_at", "2026-09-22T12:00:00.123456791Z"),
        (8, "valid_to", "malformed"),
    ] {
        db.execute_for(
            DbOperation::Execute,
            &format!("UPDATE memories SET {column} = ?1 WHERE id = ?2"),
            &[Value::Text(value.to_owned()), Value::Text(id(number))],
        )?;
    }
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, 10)?),
        [id(5), id(6), id(7)]
    );
    Ok(())
}

#[test]
fn marker_updates_are_instant_monotonic_and_offset_equality_is_read_only() -> TestResult {
    let db = DbConnection::open_memory()?;
    setup(&db)?;
    for number in 1..=2 {
        insert(&db, number, BASE, &input())?;
    }
    for (number, column) in [(1, EndColumn::ValidTo), (2, EndColumn::SupersededAt)] {
        let apply = |raw| tighten_end(&db, &id(number), raw, column);
        assert!(apply("2026-09-22T12:00:00.000000900Z")?);
        let before = db.get_memory(&id(number))?.ok_or("memory missing")?;
        assert!(!apply("2026-09-22T08:00:00.000000900-04:00")?);
        assert!(!apply("2026-09-22T12:00:01Z")?);
        assert_eq!(db.get_memory(&id(number))?, Some(before));
        assert!(apply("2026-09-22T12:00:00.000000800+00:00")?);
        assert!(!apply("2026-09-22T12:00:00.000000850Z")?);
        assert!(apply("2026-09-22T12:00:00Z")?);
        assert!(!apply("2026-09-22T12:00:00.000000001Z")?);
    }
    assert_eq!(
        db.get_memory(&id(1))?
            .ok_or("memory missing")?
            .valid_to
            .as_deref(),
        Some(BASE)
    );
    assert_eq!(db.get_memory_superseded_at(&id(2))?.as_deref(), Some(BASE));
    Ok(())
}

#[test]
fn malformed_references_and_markers_fail_without_mutation() -> TestResult {
    let db = DbConnection::open_memory()?;
    setup(&db)?;
    insert(&db, 1, BASE, &input())?;
    let before = db.get_memory(&id(1))?;
    assert!(db.expire_memory_valid_to(&id(1), "invalid").is_err());
    assert!(db.mark_memory_superseded(&id(1), "invalid").is_err());
    assert!(
        db.list_recent_current_memories_for_retrieval(WORKSPACE, "invalid", 1)
            .is_err()
    );
    assert!(
        db.list_memories_valid_at(WORKSPACE, None, false, "invalid")
            .is_err()
    );
    assert!(db.list_all_tags_valid_at(WORKSPACE, "invalid").is_err());
    assert_eq!(db.get_memory(&id(1))?, before);
    assert!(!db.expire_memory_valid_to(&id(999), BASE)?);
    Ok(())
}

#[test]
fn recency_read_scope_preserves_read_only_and_caller_owned_transactions() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("store.db");
    let db = DbConnection::open_file(&path)?;
    setup(&db)?;
    insert(&db, 1, BASE, &input())?;
    db.close()?;
    let db = DbConnection::open_file_read_only(&path)?;
    db.begin_read_snapshot()?;
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, 1)?),
        [id(1)]
    );
    db.commit_read_snapshot()?;
    assert_eq!(
        identities(&db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, 1)?),
        [id(1)]
    );
    db.close()?;
    let db = DbConnection::open_file(&path)?;
    db.with_transaction(|| {
        insert(&db, 2, "2026-09-22T12:00:00.123456789Z", &input())?;
        assert_eq!(
            identities(&db.list_recent_current_memories_for_retrieval(WORKSPACE, AT, 1)?),
            [id(2)]
        );
        Ok(())
    })?;
    assert!(db.get_memory(&id(2))?.is_some());
    Ok(())
}
