//! Real primer/cache reads at explicit applicability instants, without sleeps.

use super::*;
use crate::core::global_store::{GlobalStorePaths, open_or_create_global_store};
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

type TestResult = Result<(), String>;
const WORKSPACE: &str = "wsp_00000000000000000000000061";
const CREATED: &str = "2020-01-01T00:00:00Z";
const BEFORE: &str = "2030-01-01T00:00:00.499999999Z";
const BOUNDARY: &str = "2030-01-01T00:00:00.500000000Z";
const AFTER: &str = "2030-01-01T00:00:00.500000001Z";
const LATER: &str = "2030-01-01T00:00:00.700000000Z";

fn id(number: u32) -> String {
    format!("mem_{number:026}")
}

fn fixture() -> Result<DbConnection, String> {
    let db = DbConnection::open_memory().map_err(|error| error.to_string())?;
    db.migrate().map_err(|error| error.to_string())?;
    db.insert_workspace(
        WORKSPACE,
        &CreateWorkspaceInput {
            path: "/primer-validity-admission".to_owned(),
            name: None,
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(db)
}

fn settings() -> PrimerSettings {
    PrimerSettings {
        budget_tokens: 2000,
        format: PrimerFormat::Markdown,
        config_hash: primer_config_hash(2000, true, false),
        redact_secrets: true,
        keyword_gate_value_only: false,
        global_lane_enabled: true,
    }
}

fn insert(
    db: &DbConnection,
    workspace: &str,
    number: u32,
    content: &str,
    valid_from: Option<&str>,
    valid_to: Option<&str>,
) -> TestResult {
    let memory_id = id(number);
    db.insert_memory_with_timestamps(
        &memory_id,
        &CreateMemoryInput {
            workspace_id: workspace.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.8,
            importance: 0.7,
            provenance_uri: Some("manual://primer-validity".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: valid_from.map(str::to_owned),
            valid_to: valid_to.map(str::to_owned),
        },
        CREATED,
        CREATED,
        &memory_id,
    )
    .map_err(|error| error.to_string())?;
    if valid_from.is_none() {
        // Native captures derive a start from created_at. Preserve a genuine
        // legacy open-start control rather than accidentally testing that default.
        db.set_memory_validity_for_diagnostic(&memory_id, None, None, true, false)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn primer_at(
    db: &DbConnection,
    instant: &str,
    refresh: bool,
    persist: bool,
    global_paths: Option<&GlobalStorePaths>,
) -> Result<PrimerReport, String> {
    let reference_time = DateTime::parse_from_rfc3339(instant)
        .map_err(|error| error.to_string())?
        .with_timezone(&Utc);
    run_primer_with_global_lane_at(
        db,
        WORKSPACE,
        &settings(),
        refresh,
        persist,
        global_paths,
        reference_time,
    )
    .map_err(|error| error.to_string())
}

fn ids(report: &PrimerReport) -> BTreeSet<String> {
    report
        .sections
        .iter()
        .flat_map(|section| &section.items)
        .map(|item| item.memory_id.clone())
        .collect()
}

#[test]
fn primer_admits_only_current_valid_windows_including_exact_offset_endpoints() -> TestResult {
    let db = fixture()?;
    for (number, start, end) in [
        (1, Some(CREATED), None),
        (2, Some(CREATED), Some("2029-01-01T00:00:00Z")),
        (3, Some("2031-01-01T00:00:00Z"), None),
        (4, Some("not-a-timestamp"), None),
        (5, Some(CREATED), Some("not-a-timestamp")),
        (
            6,
            Some("2031-01-01T00:00:00Z"),
            Some("2029-01-01T00:00:00Z"),
        ),
        (7, None, None),
        (
            8,
            Some("2029-12-31T19:00:00.500000000-05:00"),
            Some("2030-01-01T02:00:00.500000000+02:00"),
        ),
        (9, Some(AFTER), None),
        (10, Some(CREATED), Some(BEFORE)),
        (11, Some(CREATED), None),
        (12, Some(CREATED), None),
    ] {
        insert(
            &db,
            WORKSPACE,
            number,
            &format!("Follow reviewed release practice {number}."),
            start,
            end,
        )?;
    }
    db.tombstone_memory(&id(11))
        .map_err(|error| error.to_string())?;
    db.restore_imported_memory_supersession(&id(12), CREATED)
        .map_err(|error| error.to_string())?;

    let cold = primer_at(&db, BOUNDARY, false, true, None)?;
    assert!(!cold.cache_hit);
    assert_eq!(ids(&cold), BTreeSet::from([id(1), id(7), id(8)]));
    let mut warm = primer_at(&db, BOUNDARY, false, true, None)?;
    assert!(warm.cache_hit);
    warm.cache_hit = false;
    assert_eq!(warm, cold);
    Ok(())
}

#[test]
fn primer_cache_tracks_local_validity_transitions_without_generation_changes() -> TestResult {
    let db = fixture()?;
    insert(
        &db,
        WORKSPACE,
        1,
        "Keep reviewed release evidence.",
        Some(CREATED),
        None,
    )?;
    insert(
        &db,
        WORKSPACE,
        2,
        "Follow the outgoing deployment convention.",
        Some(CREATED),
        Some(BOUNDARY),
    )?;
    insert(
        &db,
        WORKSPACE,
        3,
        "Follow the incoming deployment convention.",
        Some(BOUNDARY),
        None,
    )?;
    let generation = db
        .get_workspace_generation(WORKSPACE)
        .map_err(|error| error.to_string())?;

    let first = primer_at(&db, "2030-01-01T00:00:00.250Z", false, true, None)?;
    assert!(!first.cache_hit);
    assert_eq!(ids(&first), BTreeSet::from([id(1), id(2)]));
    let mut same_interval = primer_at(&db, BEFORE, false, true, None)?;
    assert!(
        same_interval.cache_hit,
        "raw time must not enter the cache key"
    );
    same_interval.cache_hit = false;
    assert_eq!(same_interval, first);

    let endpoint = primer_at(&db, BOUNDARY, false, true, None)?;
    assert!(!endpoint.cache_hit);
    assert_eq!(ids(&endpoint), BTreeSet::from([id(1), id(2), id(3)]));
    assert!(primer_at(&db, BOUNDARY, false, true, None)?.cache_hit);

    let expired = primer_at(&db, AFTER, false, true, None)?;
    assert!(!expired.cache_hit);
    assert_eq!(ids(&expired), BTreeSet::from([id(1), id(3)]));
    let mut later = primer_at(&db, LATER, false, true, None)?;
    assert!(later.cache_hit);
    later.cache_hit = false;
    assert_eq!(later, expired);
    let refreshed = primer_at(&db, LATER, true, false, None)?;
    assert_eq!(refreshed, expired);
    assert_eq!(
        db.get_workspace_generation(WORKSPACE)
            .map_err(|error| error.to_string())?,
        generation
    );
    assert_eq!(
        db.count_table_rows("primer_cache")
            .map_err(|error| error.to_string())?,
        3
    );
    Ok(())
}

#[test]
fn expired_workspace_twins_do_not_hide_applicable_global_rules() -> TestResult {
    let db = fixture()?;
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let paths = GlobalStorePaths::from_root(&path.join("global"));
    let (global, global_workspace) = open_or_create_global_store(&paths)?;
    let shared = "Always retain reviewed deployment evidence.";
    insert(
        &db,
        WORKSPACE,
        1,
        "Keep the workspace release checklist.",
        Some(CREATED),
        None,
    )?;
    insert(&db, WORKSPACE, 2, shared, Some(CREATED), Some(BOUNDARY))?;
    insert(&global, &global_workspace, 70, shared, Some(CREATED), None)?;
    for (number, start, end) in [
        (71, Some("2031-01-01T00:00:00Z"), None),
        (72, Some(CREATED), Some("2029-01-01T00:00:00Z")),
        (73, Some("not-a-timestamp"), None),
    ] {
        insert(
            &global,
            &global_workspace,
            number,
            &format!("Global release rule {number}."),
            start,
            end,
        )?;
    }
    let generation = db
        .get_workspace_generation(WORKSPACE)
        .map_err(|error| error.to_string())?;
    let before = primer_at(&db, BEFORE, false, true, Some(&paths))?;
    assert_eq!(ids(&before), BTreeSet::from([id(1), id(2)]));
    assert!(primer_at(&db, BOUNDARY, false, true, Some(&paths))?.cache_hit);

    let after = primer_at(&db, AFTER, false, true, Some(&paths))?;
    assert!(!after.cache_hit);
    assert_eq!(ids(&after), BTreeSet::from([id(1), id(70)]));
    assert!(primer_at(&db, LATER, false, true, Some(&paths))?.cache_hit);
    assert_eq!(
        db.get_workspace_generation(WORKSPACE)
            .map_err(|error| error.to_string())?,
        generation
    );
    Ok(())
}

#[test]
fn global_validity_transitions_revalidate_a_warm_primer_without_local_writes() -> TestResult {
    let db = fixture()?;
    insert(
        &db,
        WORKSPACE,
        1,
        "Keep the local release review.",
        Some(CREATED),
        None,
    )?;
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let paths = GlobalStorePaths::from_root(&path.join("global"));
    let (global, global_workspace) = open_or_create_global_store(&paths)?;
    let expiry = "2030-01-01T00:00:00.750000000Z";
    insert(
        &global,
        &global_workspace,
        70,
        "Follow the temporary shared release convention.",
        Some(BOUNDARY),
        Some(expiry),
    )?;
    let generation = db
        .get_workspace_generation(WORKSPACE)
        .map_err(|error| error.to_string())?;
    let before = primer_at(&db, BEFORE, false, true, Some(&paths))?;
    assert_eq!(ids(&before), BTreeSet::from([id(1)]));
    let active = primer_at(&db, BOUNDARY, false, true, Some(&paths))?;
    assert!(!active.cache_hit);
    assert_eq!(ids(&active), BTreeSet::from([id(1), id(70)]));
    assert!(primer_at(&db, LATER, false, true, Some(&paths))?.cache_hit);
    assert!(primer_at(&db, expiry, false, true, Some(&paths))?.cache_hit);

    let mut after = primer_at(
        &db,
        "2030-01-01T00:00:00.750000001Z",
        false,
        true,
        Some(&paths),
    )?;
    assert_eq!(ids(&after), BTreeSet::from([id(1)]));
    // The admitted source set is back to its original value. That original
    // cached response is safe to reuse; the elapsed time is not its identity.
    assert!(after.cache_hit);
    after.cache_hit = false;
    assert_eq!(after, before);
    assert_eq!(
        db.get_workspace_generation(WORKSPACE)
            .map_err(|error| error.to_string())?,
        generation
    );
    Ok(())
}

#[test]
fn validity_admission_preserves_caller_snapshot_and_no_persist_reads() -> TestResult {
    let db = fixture()?;
    insert(
        &db,
        WORKSPACE,
        1,
        "Keep the current deployment review.",
        Some(CREATED),
        None,
    )?;
    insert(
        &db,
        WORKSPACE,
        2,
        "Retire the old deployment review.",
        Some(CREATED),
        Some(BOUNDARY),
    )?;
    let original = db
        .list_memories(WORKSPACE, None, true)
        .map_err(|error| error.to_string())?;
    let cache_rows = db
        .count_table_rows("primer_cache")
        .map_err(|error| error.to_string())?;
    db.begin_read_snapshot()
        .map_err(|error| error.to_string())?;
    let report = primer_at(&db, AFTER, false, false, None)?;
    assert_eq!(ids(&report), BTreeSet::from([id(1)]));
    db.commit_read_snapshot()
        .map_err(|error| error.to_string())?;
    assert_eq!(
        db.list_memories(WORKSPACE, None, true)
            .map_err(|error| error.to_string())?,
        original
    );
    assert_eq!(
        db.count_table_rows("primer_cache")
            .map_err(|error| error.to_string())?,
        cache_rows
    );
    Ok(())
}
