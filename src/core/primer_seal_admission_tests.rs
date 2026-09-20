//! Real cold/warm primer admission, including changes with no generation bump.

use super::*;
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
use crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT;

type TestResult = Result<(), String>;
const WORKSPACE: &str = "wsp_00000000000000000000000051";
const CONTROL: &str = "mem_00000000000000000000000051";
const SEALED: &str = "mem_00000000000000000000000052";

fn input(workspace: &str, text: &str) -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: workspace.to_owned(),
        level: "procedural".to_owned(),
        kind: "rule".to_owned(),
        content: text.to_owned(),
        workflow_id: None,
        confidence: 0.95,
        utility: 0.8,
        importance: 0.7,
        provenance_uri: Some("manual://primer-admission".to_owned()),
        trust_class: "human_explicit".to_owned(),
        trust_subclass: None,
        tags: Vec::new(),
        valid_from: Some("2026-01-01T00:00:00Z".to_owned()),
        valid_to: Some("2099-01-01T00:00:00Z".to_owned()),
    }
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

fn fixture() -> Result<DbConnection, String> {
    let db = DbConnection::open_memory().map_err(|e| e.to_string())?;
    db.migrate().map_err(|e| e.to_string())?;
    db.insert_workspace(
        WORKSPACE,
        &CreateWorkspaceInput {
            path: "/primer-seal-admission".to_owned(),
            name: None,
        },
    )
    .map_err(|e| e.to_string())?;
    db.insert_memory(
        CONTROL,
        &input(WORKSPACE, "Always retain reviewed deployment evidence."),
    )
    .map_err(|e| e.to_string())?;
    db.insert_memory(SEALED, &input(WORKSPACE, MEMORY_SEAL_PLACEHOLDER_CONTENT))
        .map_err(|e| e.to_string())?;
    Ok(db)
}

fn primer(db: &DbConnection, persist: bool) -> Result<PrimerReport, String> {
    run_primer_with_global_lane(db, WORKSPACE, &settings(), false, persist, None)
        .map_err(|e| e.to_string())
}

fn ids(report: &PrimerReport) -> BTreeSet<&str> {
    report
        .sections
        .iter()
        .flat_map(|section| &section.items)
        .map(|item| item.memory_id.as_str())
        .collect()
}

fn seal(db: &DbConnection, id: &str) -> TestResult {
    db.insert_memory_seal(
        id,
        &format!("blake3:{}", "a".repeat(64)),
        "2026-06-01T00:00:00Z",
    )
    .map_err(|e| e.to_string())
}

#[test]
fn primer_seal_only_change_invalidates_a_warm_cache_without_generation_change() -> TestResult {
    let db = fixture()?;
    let cold = primer(&db, true)?;
    assert_eq!(ids(&cold), BTreeSet::from([CONTROL, SEALED]));
    let warm = primer(&db, true)?;
    assert!(warm.cache_hit);
    assert_eq!(warm.sections, cold.sections);
    let generation = db
        .get_workspace_generation(WORKSPACE)
        .map_err(|e| e.to_string())?;
    seal(&db, SEALED)?;
    assert_eq!(
        db.get_workspace_generation(WORKSPACE)
            .map_err(|e| e.to_string())?,
        generation
    );
    let admitted = primer(&db, true)?;
    assert!(!admitted.cache_hit);
    assert_eq!(ids(&admitted), BTreeSet::from([CONTROL]));
    assert!(
        !admitted
            .rendered_markdown
            .as_deref()
            .ok_or("markdown")?
            .contains(SEALED)
    );
    let mut repeated = primer(&db, true)?;
    assert!(repeated.cache_hit);
    repeated.cache_hit = false;
    assert_eq!(repeated, admitted);
    Ok(())
}

#[test]
fn primer_cold_read_and_reveal_distinguish_seal_state_from_placeholder_text() -> TestResult {
    let db = fixture()?;
    seal(&db, SEALED)?;
    assert_eq!(ids(&primer(&db, true)?), BTreeSet::from([CONTROL]));
    assert!(
        db.mark_memory_seal_revealed(SEALED, "2026-07-01T00:00:00Z")
            .map_err(|e| e.to_string())?
    );
    assert_eq!(ids(&primer(&db, true)?), BTreeSet::from([CONTROL, SEALED]));
    assert!(primer(&db, true)?.cache_hit);
    Ok(())
}

#[test]
fn primer_refuses_malformed_seal_authority_even_when_an_older_cache_is_available() -> TestResult {
    let db = fixture()?;
    assert_eq!(ids(&primer(&db, true)?), BTreeSet::from([CONTROL, SEALED]));
    assert!(primer(&db, true)?.cache_hit);
    // A closed seal around exposed text is malformed authoritative state.
    // The query must not fall back to the otherwise-valid cached body.
    seal(&db, CONTROL)?;
    let before = db
        .count_table_rows("primer_cache")
        .map_err(|e| e.to_string())?;
    let error = primer(&db, true).expect_err("bad seal must not return cached evidence");
    assert!(error.contains("Could not verify primer seal authority"));
    assert!(!error.contains("reviewed deployment evidence"));
    assert!(!error.contains(CONTROL));
    assert_eq!(
        db.count_table_rows("primer_cache")
            .map_err(|e| e.to_string())?,
        before
    );
    Ok(())
}

#[test]
fn primer_read_only_admission_preserves_source_cache_and_caller_snapshot() -> TestResult {
    let db = fixture()?;
    seal(&db, SEALED)?;
    let original = db
        .list_memories(WORKSPACE, None, true)
        .map_err(|e| e.to_string())?;
    let cache = db
        .count_table_rows("primer_cache")
        .map_err(|e| e.to_string())?;
    db.begin_read_snapshot().map_err(|e| e.to_string())?;
    let report = primer(&db, false)?;
    assert_eq!(ids(&report), BTreeSet::from([CONTROL]));
    db.commit_read_snapshot().map_err(|e| e.to_string())?;
    assert_eq!(
        db.list_memories(WORKSPACE, None, true)
            .map_err(|e| e.to_string())?,
        original
    );
    assert_eq!(
        db.count_table_rows("primer_cache")
            .map_err(|e| e.to_string())?,
        cache
    );
    Ok(())
}

#[test]
fn global_retirement_and_sealing_invalidate_primers_without_workspace_writes() -> TestResult {
    use crate::core::global_store::{GlobalStorePaths, open_or_create_global_store};
    let local = fixture()?;
    seal(&local, SEALED)?;
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = root.path().canonicalize().map_err(|e| e.to_string())?;
    let paths = GlobalStorePaths::from_root(&path.join("global"));
    let (global, workspace) = open_or_create_global_store(&paths)?;
    let global_id = "mem_00000000000000000000000053";
    global
        .insert_memory(
            global_id,
            &input(
                &workspace,
                "Global release practice differs across projects.",
            ),
        )
        .map_err(|e| e.to_string())?;
    let run = || {
        run_primer_with_global_lane(&local, WORKSPACE, &settings(), false, true, Some(&paths))
            .map_err(|e| e.to_string())
    };
    let before = run()?;
    assert_eq!(ids(&before), BTreeSet::from([CONTROL, global_id]));
    assert!(run()?.cache_hit);
    let generation = local
        .get_workspace_generation(WORKSPACE)
        .map_err(|e| e.to_string())?;
    global
        .restore_imported_memory_supersession(global_id, "2026-07-01T00:00:00Z")
        .map_err(|e| e.to_string())?;
    let after = run()?;
    assert_eq!(ids(&after), BTreeSet::from([CONTROL]));
    assert_eq!(
        local
            .get_workspace_generation(WORKSPACE)
            .map_err(|e| e.to_string())?,
        generation
    );
    assert!(run()?.cache_hit);
    let global_sealed = "mem_00000000000000000000000055";
    global
        .insert_memory(
            global_sealed,
            &input(&workspace, MEMORY_SEAL_PLACEHOLDER_CONTENT),
        )
        .map_err(|e| e.to_string())?;
    assert_eq!(ids(&run()?), BTreeSet::from([CONTROL, global_sealed]));
    assert!(run()?.cache_hit);
    let global_generation = global
        .get_workspace_generation(&workspace)
        .map_err(|e| e.to_string())?;
    seal(&global, global_sealed)?;
    assert_eq!(
        global
            .get_workspace_generation(&workspace)
            .map_err(|e| e.to_string())?,
        global_generation
    );
    assert_eq!(ids(&run()?), BTreeSet::from([CONTROL]));
    assert_eq!(
        local
            .get_workspace_generation(WORKSPACE)
            .map_err(|e| e.to_string())?,
        generation
    );
    Ok(())
}

#[test]
fn primer_revision_history_stays_retired_while_expiring_current_rules_remain_visible() -> TestResult
{
    let db = fixture()?;
    seal(&db, SEALED)?;
    let original = primer(&db, true)?;
    assert_eq!(ids(&original), BTreeSet::from([CONTROL]));
    let history = "mem_00000000000000000000000054";
    db.insert_memory(
        history,
        &input(WORKSPACE, "Retain this old procedure as history only."),
    )
    .map_err(|e| e.to_string())?;
    db.restore_imported_memory_supersession(history, "2026-07-01T00:00:00Z")
        .map_err(|e| e.to_string())?;
    assert_eq!(ids(&primer(&db, false)?), BTreeSet::from([CONTROL]));
    assert!(db.get_memory(history).map_err(|e| e.to_string())?.is_some());
    assert_eq!(
        db.get_memory(CONTROL)
            .map_err(|e| e.to_string())?
            .ok_or("control")?
            .valid_to
            .as_deref(),
        Some("2099-01-01T00:00:00Z")
    );
    Ok(())
}

#[test]
fn historical_global_admission_does_not_require_including_tombstones() -> TestResult {
    use crate::core::global_store::{
        GlobalStorePaths, open_or_create_global_store, read_global_store_memories_at,
    };
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = root.path().canonicalize().map_err(|e| e.to_string())?;
    let paths = GlobalStorePaths::from_root(&path.join("global"));
    let (db, workspace) = open_or_create_global_store(&paths)?;
    let prior = "mem_00000000000000000000000056";
    let head = "mem_00000000000000000000000057";
    for id in [prior, head] {
        db.insert_memory_with_timestamps(
            id,
            &input(&workspace, "Global historical recall evidence."),
            "2026-01-01T00:00:00+00:00",
            "2026-01-01T00:00:00+00:00",
            prior,
        )
        .map_err(|e| e.to_string())?;
    }
    db.restore_imported_memory_supersession(prior, "2026-06-01T00:00:00Z")
        .map_err(|e| e.to_string())?;
    for (raw, expected) in [
        ("2026-05-31T23:59:59.999999999Z", vec![prior, head]),
        ("2026-06-01T00:00:00Z", vec![head]),
    ] {
        let reference = chrono::DateTime::parse_from_rfc3339(raw)
            .map_err(|e| e.to_string())?
            .with_timezone(&chrono::Utc);
        let rows = read_global_store_memories_at(&paths, false, Some(reference))?;
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            expected
        );
    }
    assert_eq!(
        db.list_memories(&workspace, None, true)
            .map_err(|e| e.to_string())?
            .len(),
        2
    );
    Ok(())
}
