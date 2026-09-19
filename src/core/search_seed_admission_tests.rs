//! Similarity queries must not consume an obsolete or unrevealed seed body.

use super::*;
use crate::core::memory_scope::MemoryScopeContext;
use crate::core::search::{SimilarError, SimilarOptions, resolve_similar_seed_memory, run_similar};
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
use crate::models::MemoryScope;
use crate::search::SpeedMode;

type TestResult = Result<(), String>;
const WORKSPACE: &str = "wsp_00000000000000000000000031";
const MEMORY: &str = "mem_00000000000000000000000031";
const CUTOFF: &str = "2026-06-01T00:00:00Z";

fn at(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .expect("fixture timestamp")
        .with_timezone(&Utc)
}

fn fixture() -> Result<(tempfile::TempDir, DbConnection, SimilarOptions), String> {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = root.path().canonicalize().map_err(|e| e.to_string())?;
    let database = path.join("ee.db");
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    db.migrate().map_err(|e| e.to_string())?;
    db.insert_workspace(
        WORKSPACE,
        &CreateWorkspaceInput {
            path: path.to_string_lossy().into_owned(),
            name: None,
        },
    )
    .map_err(|e| e.to_string())?;
    db.insert_memory_with_timestamps(
        MEMORY,
        &CreateMemoryInput {
            workspace_id: WORKSPACE.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: "Use the reviewed release procedure.".to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.7,
            importance: 0.8,
            provenance_uri: Some("manual://seed-admission".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: Some("2026-01-01T00:00:00Z".to_owned()),
            valid_to: Some("2099-01-01T00:00:00Z".to_owned()),
        },
        "2026-01-01T00:00:00+00:00",
        "2026-01-01T00:00:00+00:00",
        MEMORY,
    )
    .map_err(|e| e.to_string())?;
    let options = SimilarOptions {
        workspace_path: path.clone(),
        database_path: Some(database),
        index_dir: Some(path.join("absent-index")),
        memory_id: MEMORY.to_owned(),
        limit: 5,
        min_score: Some(0.0),
        speed: SpeedMode::Default,
        explain: true,
        as_of: Some(at("2026-08-01T00:00:00Z")),
        include_tombstoned: true,
        include_expired: true,
        include_future: true,
        include_stale: true,
        memory_scope: MemoryScope::Workspace,
        strict_scope: false,
    };
    Ok((root, db, options))
}

fn resolve(
    db: &DbConnection,
    options: &SimilarOptions,
) -> Result<crate::db::StoredMemory, SimilarError> {
    let scope = MemoryScopeContext::for_workspace_with_connection(
        &options.workspace_path,
        options.memory_scope,
        options.strict_scope,
        Some(db),
    );
    resolve_similar_seed_memory(db, options, WORKSPACE, &scope)
}

#[test]
fn similarity_seed_respects_exact_supersession_despite_permissive_flags() -> TestResult {
    let (_root, db, mut options) = fixture()?;
    assert!(resolve(&db, &options).is_ok());
    assert!(
        db.restore_imported_memory_supersession(MEMORY, CUTOFF)
            .map_err(|e| e.to_string())?
    );
    for instant in [CUTOFF, "2026-08-01T00:00:00Z"] {
        options.as_of = Some(at(instant));
        assert!(matches!(
            resolve(&db, &options),
            Err(SimilarError::MemoryNotFound { .. })
        ));
    }
    options.as_of = Some(at("2026-05-31T23:59:59.999999999Z"));
    let memory = resolve(&db, &options).map_err(|e| e.to_string())?;
    assert_eq!(memory.id, MEMORY);
    assert_eq!(memory.valid_to.as_deref(), Some("2099-01-01T00:00:00Z"));
    Ok(())
}

#[test]
fn similarity_seed_seal_is_authority_not_placeholder_text() -> TestResult {
    let (_root, db, options) = fixture()?;
    let commitment = format!("blake3:{}", "a".repeat(64));
    // Even an unexpectedly populated body must not defeat a closed seal.
    db.insert_memory_seal(MEMORY, &commitment, "2026-01-01T00:00:00Z")
        .map_err(|e| e.to_string())?;
    assert!(matches!(
        resolve(&db, &options),
        Err(SimilarError::MemoryNotFound { .. })
    ));
    assert!(
        db.mark_memory_seal_revealed(MEMORY, "2026-02-01T00:00:00Z")
            .map_err(|e| e.to_string())?
    );
    assert_eq!(
        resolve(&db, &options).map_err(|e| e.to_string())?.id,
        MEMORY
    );
    Ok(())
}

#[test]
fn public_similar_refuses_hidden_seeds_before_creating_or_opening_an_index() -> TestResult {
    for sealed in [false, true] {
        let (_root, db, options) = fixture()?;
        if sealed {
            db.insert_memory_seal(MEMORY, &format!("blake3:{}", "a".repeat(64)), CUTOFF)
                .map_err(|e| e.to_string())?;
        } else {
            db.restore_imported_memory_supersession(MEMORY, CUTOFF)
                .map_err(|e| e.to_string())?;
        }
        let before = db.get_memory(MEMORY).map_err(|e| e.to_string())?;
        let audits = db
            .count_table_rows("audit_log")
            .map_err(|e| e.to_string())?;
        assert!(matches!(
            run_similar(&options),
            Err(SimilarError::MemoryNotFound { .. })
        ));
        assert_eq!(db.get_memory(MEMORY).map_err(|e| e.to_string())?, before);
        assert_eq!(
            db.count_table_rows("audit_log")
                .map_err(|e| e.to_string())?,
            audits
        );
        assert!(!options.index_dir.as_ref().ok_or("index path")?.exists());
    }
    Ok(())
}

#[test]
fn seed_revision_and_body_obey_the_same_real_read_snapshot() -> TestResult {
    let (_root, writer, options) = fixture()?;
    let reader =
        DbConnection::open_file_read_only(options.database_path.as_ref().ok_or("database")?)
            .map_err(|e| e.to_string())?;
    let snapshot = RevisionReadSnapshot::begin(&reader).map_err(|e| e.to_string())?;
    let original = resolve(&reader, &options).map_err(|e| e.to_string())?;
    writer
        .restore_imported_memory_supersession(MEMORY, CUTOFF)
        .map_err(|e| e.to_string())?;
    assert_eq!(
        resolve(&reader, &options).map_err(|e| e.to_string())?,
        original
    );
    snapshot.finish().map_err(|e| e.to_string())?;
    assert!(matches!(
        resolve(&reader, &options),
        Err(SimilarError::MemoryNotFound { .. })
    ));
    Ok(())
}

#[test]
fn seed_snapshot_guard_preserves_caller_transaction_and_releases_owned_reads() -> TestResult {
    let (_root, db, options) = fixture()?;
    db.begin_read_snapshot().map_err(|e| e.to_string())?;
    assert!(RevisionReadSnapshot::begin(&db).is_err());
    assert!(resolve(&db, &options).is_ok());
    db.commit_read_snapshot().map_err(|e| e.to_string())?;
    {
        let _snapshot = RevisionReadSnapshot::begin(&db).map_err(|e| e.to_string())?;
        assert!(resolve(&db, &options).is_ok());
    }
    db.begin_read_snapshot().map_err(|e| e.to_string())?;
    db.rollback_read_snapshot().map_err(|e| e.to_string())?;
    Ok(())
}

#[test]
fn malformed_similarity_revision_fails_without_echoing_the_source_value() -> TestResult {
    let (_root, db, options) = fixture()?;
    db.execute_raw("UPDATE memories SET superseded_at = 'PRIVATE-REVISION-CANARY'")
        .map_err(|e| e.to_string())?;
    let error = resolve(&db, &options).expect_err("malformed authority must be refused");
    assert!(!error.to_string().contains("PRIVATE-REVISION-CANARY"));
    assert!(
        error
            .to_string()
            .contains("Could not verify similarity seed revision state")
    );
    Ok(())
}
