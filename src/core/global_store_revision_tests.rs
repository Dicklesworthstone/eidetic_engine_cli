//! Real global-store authority reads, including historical and primer semantics.

use super::*;
use crate::db::{CreateFeedbackQuarantineInput, CreateMemoryInput, CreateWorkspaceInput};
use chrono::{DateTime, Utc};

const PRIOR: &str = "mem_00000000000000000000000041";
const HEAD: &str = "mem_00000000000000000000000042";
const SEALED: &str = "mem_00000000000000000000000043";
const CUTOFF: &str = "2026-06-01T00:00:00Z";
type TestResult = Result<(), String>;

fn at(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .expect("fixture timestamp")
        .with_timezone(&Utc)
}

fn input(workspace: &str, content: &str) -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: workspace.to_owned(),
        level: "procedural".to_owned(),
        kind: "rule".to_owned(),
        content: content.to_owned(),
        workflow_id: None,
        confidence: 0.9,
        utility: 0.8,
        importance: 0.7,
        provenance_uri: Some("manual://global-revision".to_owned()),
        trust_class: "human_explicit".to_owned(),
        trust_subclass: None,
        tags: vec![crate::models::GLOBAL_MEMORY_SCOPE_TAG.to_owned()],
        valid_from: Some("2026-01-01T00:00:00Z".to_owned()),
        valid_to: Some("2099-01-01T00:00:00Z".to_owned()),
    }
}

fn fixture() -> Result<(tempfile::TempDir, GlobalStorePaths, DbConnection, String), String> {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = root.path().canonicalize().map_err(|e| e.to_string())?;
    let paths = GlobalStorePaths::from_root(&path.join("global"));
    let (db, workspace) = open_or_create_global_store(&paths)?;
    for (id, body) in [
        (PRIOR, "Legacy deployment advice."),
        (HEAD, "Reviewed deployment advice."),
    ] {
        db.insert_memory_with_timestamps(
            id,
            &input(&workspace, body),
            "2026-01-01T00:00:00+00:00",
            "2026-01-01T00:00:00+00:00",
            PRIOR,
        )
        .map_err(|e| e.to_string())?;
    }
    db.restore_imported_memory_supersession(PRIOR, CUTOFF)
        .map_err(|e| e.to_string())?;
    Ok((root, paths, db, workspace))
}

fn ids(rows: &[StoredMemory]) -> Vec<&str> {
    let mut ids = rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

fn hold(
    db: &DbConnection,
    number: u32,
    workspace: &str,
    target_type: &str,
    target: &str,
) -> Result<String, String> {
    let id = format!("fq_{number:026}");
    db.insert_feedback_quarantine(
        &id,
        &CreateFeedbackQuarantineInput {
            workspace_id: workspace.to_owned(),
            source_id: "PRIVATE-GLOBAL-REVIEW-SOURCE".to_owned(),
            target_type: target_type.to_owned(),
            target_id: target.to_owned(),
            signal: "harmful".to_owned(),
            weight: 1.0,
            source_type: "outcome_observed".to_owned(),
            proposed_event_id: None,
            recorded_at: "2026-01-01T00:00:00Z".to_owned(),
            reason: "PRIVATE-GLOBAL-REVIEW-REASON".to_owned(),
            event_reason: None,
            evidence_json: None,
            session_id: None,
            raw_event_hash: format!("blake3:{}", "a".repeat(64)),
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

#[test]
fn global_current_reads_exclude_history_without_changing_author_expiry() -> TestResult {
    let (_root, paths, db, workspace) = fixture()?;
    let current = read_global_store_memories(&paths, true)?;
    assert_eq!(ids(&current), [HEAD]);
    assert_eq!(current[0].valid_to.as_deref(), Some("2099-01-01T00:00:00Z"));
    assert_eq!(
        db.list_memories(&workspace, None, true)
            .map_err(|e| e.to_string())?
            .len(),
        2
    );
    assert_eq!(
        db.get_memory(PRIOR)
            .map_err(|e| e.to_string())?
            .ok_or("prior")?
            .content,
        "Legacy deployment advice."
    );
    Ok(())
}

#[test]
fn global_historical_reads_use_the_exact_exclusive_supersession_boundary() -> TestResult {
    let (_root, paths, _db, _workspace) = fixture()?;
    let before =
        read_global_store_memories_at(&paths, true, Some(at("2026-05-31T23:59:59.999999999Z")))?;
    // This API gates revision authority only. The search caller additionally
    // applies each row's author validity interval, including future starts.
    assert_eq!(ids(&before), [PRIOR, HEAD]);
    for reference in [CUTOFF, "2026-08-01T00:00:00Z", "2026-06-01T01:00:00+01:00"] {
        assert_eq!(
            ids(&read_global_store_memories_at(
                &paths,
                true,
                Some(at(reference))
            )?),
            [HEAD]
        );
    }
    Ok(())
}

#[test]
fn global_seals_cannot_be_bypassed_by_current_or_historical_reads() -> TestResult {
    let (_root, paths, db, workspace) = fixture()?;
    db.insert_memory(
        SEALED,
        &input(&workspace, crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT),
    )
    .map_err(|e| e.to_string())?;
    db.insert_memory_seal(
        SEALED,
        &format!("blake3:{}", "a".repeat(64)),
        "2026-01-01T00:00:00Z",
    )
    .map_err(|e| e.to_string())?;
    for reference in [None, Some(at("2026-05-01T00:00:00Z"))] {
        assert!(!ids(&read_global_store_memories_at(&paths, true, reference)?).contains(&SEALED));
    }
    assert!(
        db.mark_memory_seal_revealed(SEALED, CUTOFF)
            .map_err(|e| e.to_string())?
    );
    assert_eq!(
        ids(&read_global_store_memories(&paths, true)?),
        [HEAD, SEALED]
    );
    Ok(())
}

#[test]
fn global_reader_preserves_tombstone_flags_and_workspace_ownership() -> TestResult {
    let (_root, paths, db, _workspace) = fixture()?;
    let foreign = "wsp_00000000000000000000000044";
    db.insert_workspace(
        foreign,
        &CreateWorkspaceInput {
            path: paths.root.join("other").to_string_lossy().into_owned(),
            name: None,
        },
    )
    .map_err(|e| e.to_string())?;
    db.insert_memory(SEALED, &input(foreign, "Foreign workspace advice."))
        .map_err(|e| e.to_string())?;
    assert_eq!(ids(&read_global_store_memories(&paths, false)?), [HEAD]);
    db.tombstone_memory(HEAD).map_err(|e| e.to_string())?;
    assert!(read_global_store_memories(&paths, false)?.is_empty());
    assert_eq!(ids(&read_global_store_memories(&paths, true)?), [HEAD]);
    Ok(())
}

#[test]
fn global_review_holds_apply_to_current_and_historical_reads_without_mutation() -> TestResult {
    let (_root, paths, db, workspace) = fixture()?;
    db.insert_memory(SEALED, &input(&workspace, "Uncontested global guidance."))
        .map_err(|e| e.to_string())?;
    let historical = Some(at("2026-05-01T00:00:00Z"));
    assert_eq!(
        ids(&read_global_store_memories_at(&paths, true, historical)?),
        [PRIOR, HEAD, SEALED]
    );
    hold(&db, 1, &workspace, "memory", PRIOR)?;
    hold(&db, 2, &workspace, "memory", HEAD)?;
    let rows = db
        .list_memories(&workspace, None, true)
        .map_err(|e| e.to_string())?;
    let generation = db
        .get_workspace_generation(&workspace)
        .map_err(|e| e.to_string())?;
    let audits = db
        .count_table_rows("audit_log")
        .map_err(|e| e.to_string())?;
    for include_tombstoned in [false, true] {
        for reference in [None, historical, Some(at(CUTOFF))] {
            let admitted = read_global_store_memories_at(&paths, include_tombstoned, reference)?;
            assert_eq!(ids(&admitted), [SEALED]);
            assert_eq!(admitted[0].content, "Uncontested global guidance.");
        }
    }
    assert_eq!(
        db.list_memories(&workspace, None, true)
            .map_err(|e| e.to_string())?,
        rows
    );
    assert_eq!(
        db.get_workspace_generation(&workspace)
            .map_err(|e| e.to_string())?,
        generation
    );
    assert_eq!(
        db.count_table_rows("audit_log")
            .map_err(|e| e.to_string())?,
        audits
    );
    Ok(())
}

#[test]
fn global_review_release_requires_all_matching_holds_to_be_resolved() -> TestResult {
    let (_root, paths, db, workspace) = fixture()?;
    let foreign = "wsp_00000000000000000000000044";
    db.insert_workspace(
        foreign,
        &CreateWorkspaceInput {
            path: paths.root.join("other").to_string_lossy().into_owned(),
            name: None,
        },
    )
    .map_err(|e| e.to_string())?;
    hold(&db, 1, foreign, "memory", HEAD)?;
    hold(&db, 2, &workspace, "rule", HEAD)?;
    assert_eq!(ids(&read_global_store_memories(&paths, false)?), [HEAD]);
    let first = hold(&db, 3, &workspace, "memory", HEAD)?;
    let second = hold(&db, 4, &workspace, "memory", HEAD)?;
    assert!(read_global_store_memories(&paths, false)?.is_empty());
    assert!(
        db.update_feedback_quarantine_status(&first, "released", Some("operator"), None)
            .map_err(|e| e.to_string())?
    );
    assert!(read_global_store_memories(&paths, false)?.is_empty());
    assert!(
        db.update_feedback_quarantine_status(&second, "rejected", Some("operator"), None)
            .map_err(|e| e.to_string())?
    );
    assert_eq!(ids(&read_global_store_memories(&paths, false)?), [HEAD]);
    // Resolving review does not override another live exclusion.
    db.tombstone_memory(HEAD).map_err(|e| e.to_string())?;
    assert!(read_global_store_memories(&paths, false)?.is_empty());
    Ok(())
}

#[test]
fn global_review_changes_preserve_the_callers_read_snapshot() -> TestResult {
    let (_root, paths, writer, workspace) = fixture()?;
    let reader =
        DbConnection::open_file_read_only(&paths.database_path).map_err(|e| e.to_string())?;
    reader.begin_read_snapshot().map_err(|e| e.to_string())?;
    let before = read_global_rows_in_snapshot(&reader, &paths, false, None)?;
    assert_eq!(ids(&before), [HEAD]);
    let pending = hold(&writer, 1, &workspace, "memory", HEAD)?;
    assert_eq!(
        read_global_rows_in_snapshot(&reader, &paths, false, None)?,
        before
    );
    reader.commit_read_snapshot().map_err(|e| e.to_string())?;
    reader.begin_read_snapshot().map_err(|e| e.to_string())?;
    assert!(read_global_rows_in_snapshot(&reader, &paths, false, None)?.is_empty());
    assert!(
        writer
            .update_feedback_quarantine_status(&pending, "released", Some("operator"), None)
            .map_err(|e| e.to_string())?
    );
    assert!(read_global_rows_in_snapshot(&reader, &paths, false, None)?.is_empty());
    reader.commit_read_snapshot().map_err(|e| e.to_string())?;
    assert_eq!(ids(&read_global_store_memories(&paths, false)?), [HEAD]);
    Ok(())
}

#[test]
fn unreadable_global_review_authority_withholds_the_lane_without_private_details() -> TestResult {
    let (_root, paths, db, workspace) = fixture()?;
    hold(&db, 1, &workspace, "memory", HEAD)?;
    db.execute_raw("ALTER TABLE feedback_quarantine RENAME TO PRIVATE_GLOBAL_REVIEW_AUTHORITY")
        .map_err(|e| e.to_string())?;
    let before = std::fs::read(&paths.database_path).map_err(|e| e.to_string())?;
    for reference in [None, Some(at("2026-05-01T00:00:00Z"))] {
        let error = read_global_store_memories_at(&paths, true, reference)
            .expect_err("missing review authority must withhold the global lane");
        assert_eq!(error, global_read_error("ignored"));
        assert!(!error.contains("PRIVATE"));
        assert!(!error.contains(&paths.database_path.to_string_lossy().into_owned()));
    }
    assert_eq!(
        std::fs::read(&paths.database_path).map_err(|e| e.to_string())?,
        before
    );
    Ok(())
}

#[test]
fn global_authority_uses_one_real_snapshot_without_owning_the_callers_transaction() -> TestResult {
    let (_root, paths, writer, _workspace) = fixture()?;
    let reader =
        DbConnection::open_file_read_only(&paths.database_path).map_err(|e| e.to_string())?;
    reader.begin_read_snapshot().map_err(|e| e.to_string())?;
    let old = read_global_rows_in_snapshot(&reader, &paths, true, None)?;
    assert_eq!(ids(&old), [HEAD]);
    writer
        .restore_imported_memory_supersession(HEAD, "2026-07-01T00:00:00Z")
        .map_err(|e| e.to_string())?;
    assert_eq!(
        read_global_rows_in_snapshot(&reader, &paths, true, None)?,
        old
    );
    reader.commit_read_snapshot().map_err(|e| e.to_string())?;
    assert!(read_global_store_memories(&paths, true)?.is_empty());
    Ok(())
}

#[test]
fn malformed_global_revision_withholds_the_lane_without_echoing_private_values() -> TestResult {
    let (_root, paths, db, _workspace) = fixture()?;
    db.execute_raw("UPDATE memories SET superseded_at = 'PRIVATE-GLOBAL-REVISION-CANARY' WHERE superseded_at IS NOT NULL")
        .map_err(|e| e.to_string())?;
    let before = std::fs::read(&paths.database_path).map_err(|e| e.to_string())?;
    for reference in [None, Some(at(CUTOFF))] {
        let error =
            read_global_store_memories_at(&paths, true, reference).expect_err("malformed revision");
        assert!(!error.contains("PRIVATE-GLOBAL-REVISION-CANARY"));
        assert!(!error.contains(&paths.database_path.to_string_lossy().into_owned()));
        assert!(error.contains("optional global lane was withheld"));
    }
    assert_eq!(
        std::fs::read(&paths.database_path).map_err(|e| e.to_string())?,
        before
    );
    Ok(())
}

#[test]
fn absent_global_read_is_non_mutating_for_current_and_historical_queries() -> TestResult {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let paths = GlobalStorePaths::from_root(&root.path().join("absent"));
    assert!(read_global_store_memories(&paths, true)?.is_empty());
    assert!(read_global_store_memories_at(&paths, true, Some(at(CUTOFF)))?.is_empty());
    assert!(!paths.root.exists());
    Ok(())
}
