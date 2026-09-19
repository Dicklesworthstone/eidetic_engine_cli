//! Real-store checks for pre-ranking revision admission.

use super::super::super::{
    ScoreSource, SearchDedupMode, SearchSourceMode, SpeedMode,
    apply_tombstone_visibility_with_connection,
};
use super::*;
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
use crate::models::{MemoryScope, WorkspaceId};
use serde_json::json;
use uuid::Uuid;

type TestResult = Result<(), String>;
const WORKSPACE: &str = "wsp_00000000000000000000000011";
const PRIOR: &str = "mem_00000000000000000000000021";
const HEAD: &str = "mem_00000000000000000000000022";

fn instant(raw: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(raw)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|e| e.to_string())
}

fn memory(content: &str, from: &str) -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: WORKSPACE.to_owned(),
        level: "semantic".to_owned(),
        kind: "fact".to_owned(),
        content: content.to_owned(),
        workflow_id: None,
        confidence: 0.9,
        utility: 0.5,
        importance: 0.6,
        provenance_uri: Some("manual://revision-admission".to_owned()),
        trust_class: "agent_assertion".to_owned(),
        trust_subclass: None,
        tags: Vec::new(),
        valid_from: Some(from.to_owned()),
        valid_to: Some("2099-01-01T00:00:00Z".to_owned()),
    }
}

fn fixture() -> Result<(tempfile::TempDir, SearchOptions, DbConnection), String> {
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = temp.path().canonicalize().map_err(|e| e.to_string())?;
    std::fs::create_dir(root.join(".ee")).map_err(|e| e.to_string())?;
    std::fs::write(
        root.join(".ee/config.toml"),
        "[memory]\ninclude_global = false\n",
    )
    .map_err(|e| e.to_string())?;
    let database = root.join("revisions.db");
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    db.migrate().map_err(|e| e.to_string())?;
    // The fixed IDs are canonical and are not merely prefix-shaped fixtures.
    WORKSPACE
        .parse::<WorkspaceId>()
        .map_err(|e| e.to_string())?;
    db.insert_workspace(
        WORKSPACE,
        &CreateWorkspaceInput {
            path: root.to_string_lossy().into_owned(),
            name: None,
        },
    )
    .map_err(|e| e.to_string())?;
    for (id, created, content) in [
        (
            PRIOR,
            "2026-05-01T00:00:00Z",
            "Original deployment procedure.",
        ),
        (
            HEAD,
            "2026-06-01T00:00:00Z",
            "Corrected deployment procedure.",
        ),
    ] {
        id.parse::<MemoryId>().map_err(|e| e.to_string())?;
        db.insert_memory_with_timestamps(id, &memory(content, created), created, created, PRIOR)
            .map_err(|e| e.to_string())?;
    }
    assert!(
        db.restore_imported_memory_supersession(PRIOR, "2026-06-01T00:00:00Z")
            .map_err(|e| e.to_string())?
    );
    let options = SearchOptions {
        workspace_path: root.clone(),
        database_path: Some(database),
        index_dir: Some(root.join("index")),
        query: "deployment procedure".to_owned(),
        limit: 10,
        speed: SpeedMode::Default,
        explain: false,
        as_of: Some(instant("2026-07-01T00:00:00Z")?),
        include_tombstoned: false,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: Some(0.0),
        dedup_mode: SearchDedupMode::DocId,
        source_mode: SearchSourceMode::LexicalOnly,
        strict_source_mode: true,
        memory_scope: MemoryScope::Workspace,
        strict_scope: false,
    };
    Ok((temp, options, db))
}

fn hit(id: &str) -> SearchHit {
    SearchHit {
        doc_id: id.to_owned(),
        score: 0.9,
        source: ScoreSource::Lexical,
        fast_score: None,
        quality_score: None,
        lexical_score: Some(0.9),
        rerank_score: None,
        metadata: Some(json!({"superseded_at": null})),
        explanation: None,
    }
}

fn ids(hits: &[SearchHit]) -> Vec<&str> {
    hits.iter().map(|hit| hit.doc_id.as_str()).collect()
}

#[test]
fn current_and_historical_candidates_use_source_markers_not_author_expiry() -> TestResult {
    let (_temp, mut options, db) = fixture()?;
    let before = db
        .list_memories(WORKSPACE, None, true)
        .map_err(|e| e.to_string())?;
    for (reference, expected) in [
        ("2026-07-01T00:00:00Z", HEAD),
        ("2026-05-15T00:00:00Z", PRIOR),
        ("2026-06-01T00:00:00Z", HEAD),
        ("2026-06-01T01:00:00+01:00", HEAD),
        ("2026-05-31T23:59:59.999999999Z", PRIOR),
    ] {
        options.as_of = Some(instant(reference)?);
        let candidates = super::super::admit_hits(
            &options,
            vec![hit(PRIOR), hit(HEAD)],
            &mut Vec::new(),
            Some(&db),
        );
        let visible = apply_tombstone_visibility_with_connection(
            &options,
            candidates,
            &mut Vec::new(),
            &db,
            None,
        );
        assert_eq!(ids(&visible), vec![expected], "{reference}");
    }
    assert_eq!(
        db.list_memories(WORKSPACE, None, true)
            .map_err(|e| e.to_string())?,
        before
    );
    Ok(())
}

#[test]
fn inclusion_flags_and_fabricated_metadata_cannot_revive_superseded_candidates() -> TestResult {
    let (_temp, mut options, db) = fixture()?;
    options.include_tombstoned = true;
    options.include_expired = true;
    options.include_future = true;
    options.include_stale = true;
    let mut old = hit(PRIOR);
    old.metadata = Some(json!({"superseded_at": null, "current_revision": true,
        "valid_to": "2099-01-01T00:00:00Z"}));
    let mut degraded = Vec::new();
    let hits = admit_hits(
        &options,
        vec![old, hit(HEAD), hit("evd_other")],
        &mut degraded,
        Some(&db),
    );
    assert_eq!(ids(&hits), vec![HEAD, "evd_other"]);
    assert_eq!(hits[0].score, 0.9);
    assert!(degraded.iter().any(|entry| entry.code == FILTERED));
    assert!(!degraded.iter().any(|entry| entry.message.contains(PRIOR)));
    Ok(())
}

#[test]
fn author_expiry_alone_never_becomes_a_revision_marker() -> TestResult {
    let (_temp, mut options, db) = fixture()?;
    db.execute_raw(&format!(
        "UPDATE memories SET valid_to = '2026-06-15T00:00:00Z' WHERE id = '{HEAD}'"
    ))
    .map_err(|e| e.to_string())?;
    let candidates = admit_hits(&options, vec![hit(HEAD)], &mut Vec::new(), Some(&db));
    assert_eq!(ids(&candidates), vec![HEAD]);
    assert!(
        apply_tombstone_visibility_with_connection(
            &options,
            candidates.clone(),
            &mut Vec::new(),
            &db,
            None,
        )
        .is_empty()
    );
    options.include_expired = true;
    assert_eq!(
        ids(&apply_tombstone_visibility_with_connection(
            &options,
            candidates,
            &mut Vec::new(),
            &db,
            None,
        )),
        vec![HEAD]
    );
    Ok(())
}

#[test]
fn malformed_revision_metadata_is_withheld_without_echoing_private_values() -> TestResult {
    let (_temp, options, db) = fixture()?;
    db.execute_raw(&format!(
        "UPDATE memories SET superseded_at = 'PRIVATE_REVISION_CANARY' WHERE id = '{HEAD}'"
    ))
    .map_err(|e| e.to_string())?;
    let mut degraded = Vec::new();
    let hits = admit_hits(
        &options,
        vec![hit(HEAD), hit("evd_other")],
        &mut degraded,
        Some(&db),
    );
    assert_eq!(ids(&hits), vec!["evd_other"]);
    assert_eq!(degraded.len(), 1);
    assert_eq!(degraded[0].code, UNAVAILABLE);
    assert!(!degraded[0].message.contains("PRIVATE_REVISION_CANARY"));
    assert!(!degraded[0].message.contains(HEAD));
    Ok(())
}

#[test]
fn unavailable_source_never_creates_storage_or_discards_other_entity_types() -> TestResult {
    let (_temp, mut options, db) = fixture()?;
    db.close().map_err(|e| e.to_string())?;
    let missing = options.workspace_path.join("absent/ee.db");
    options.database_path = Some(missing.clone());
    let mut degraded = Vec::new();
    let hits = admit_hits(
        &options,
        vec![hit(HEAD), hit("evd_other")],
        &mut degraded,
        None,
    );
    assert_eq!(ids(&hits), vec!["evd_other"]);
    assert_eq!(degraded[0].code, UNAVAILABLE);
    assert!(!missing.parent().ok_or("parent")?.exists());
    assert!(!degraded[0].message.contains("absent"));
    Ok(())
}

#[test]
fn supplied_snapshot_is_used_without_opening_or_releasing_another_source() -> TestResult {
    let (_temp, mut options, db) = fixture()?;
    let missing = options.workspace_path.join("absent/ee.db");
    options.database_path = Some(missing.clone());
    db.begin_read_snapshot().map_err(|e| e.to_string())?;
    let hits = admit_hits(
        &options,
        vec![hit(PRIOR), hit(HEAD)],
        &mut Vec::new(),
        Some(&db),
    );
    assert_eq!(ids(&hits), vec![HEAD]);
    db.commit_read_snapshot().map_err(|e| e.to_string())?;
    assert!(!missing.parent().ok_or("parent")?.exists());
    Ok(())
}

#[test]
fn bounded_batches_preserve_candidate_order_and_defer_unknown_store_identities() -> TestResult {
    let (_temp, options, db) = fixture()?;
    let mut candidates = Vec::new();
    let mut expected = Vec::new();
    db.with_transaction(|| {
        for ordinal in 1000..(1000 + PAGE_SIZE * 2 + 1) {
            let id = MemoryId::from_uuid(Uuid::from_u128(ordinal as u128)).to_string();
            db.insert_memory(
                &id,
                &memory("Retained deployment evidence.", "2026-05-01T00:00:00Z"),
            )?;
            if ordinal % 2 == 0 {
                db.restore_imported_memory_supersession(&id, "2026-06-01T00:00:00Z")?;
            } else {
                expected.push(id.clone());
            }
            candidates.push(hit(&id));
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    let unknown = MemoryId::from_uuid(Uuid::from_u128(5000)).to_string();
    candidates.push(hit(&unknown));
    expected.push(unknown);
    candidates.reverse();
    expected.reverse();
    let actual = admit_hits(&options, candidates, &mut Vec::new(), Some(&db));
    assert_eq!(
        ids(&actual),
        expected.iter().map(String::as_str).collect::<Vec<_>>()
    );
    Ok(())
}

#[cfg(feature = "lexical-bm25")]
#[test]
fn public_search_retains_indexed_history_but_returns_only_the_revision_at_query_time() -> TestResult
{
    let (_temp, mut options, db) = fixture()?;
    db.close().map_err(|e| e.to_string())?;
    crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: options.database_path.clone(),
        index_dir: options.index_dir.clone(),
        dry_run: false,
    })
    .map_err(|e| e.to_string())?;
    let status = crate::core::index::get_index_status(&crate::core::index::IndexStatusOptions {
        workspace_path: options.workspace_path.clone(),
        database_path: options.database_path.clone(),
        index_dir: options.index_dir.clone(),
    })
    .map_err(|e| e.to_string())?;
    assert_eq!(status.index_document_count, Some(2));
    for (reference, expected) in [
        (Some("2026-05-15T00:00:00Z"), PRIOR),
        (Some("2026-06-01T00:00:00Z"), HEAD),
        (Some("2026-07-01T00:00:00Z"), HEAD),
        (None, HEAD),
    ] {
        options.as_of = reference.map(instant).transpose()?;
        let report =
            crate::core::search::run_search_unaudited(&options).map_err(|e| e.to_string())?;
        assert_eq!(
            ids(&report.results),
            vec![expected],
            "{reference:?}: {:?}",
            report.degraded
        );
    }
    Ok(())
}
