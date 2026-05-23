//! V062 migration acceptance tests for create-derived curation candidates
//! (bd-8k9gh).
//!
//! These tests cover the schema-layer contract for V062:
//!
//!   1. A freshly migrated database applies V062 without error.
//!   2. `curation_candidates_v060` is retained as migration evidence (the
//!      V060 rename target).
//!   3. The existing `insert_curation_candidate` API still accepts an
//!      ordinary target-mutating candidate after V062 (regression).
//!   4. The migration count reflects the new ledger entry.
//!
//! Negative CHECK-constraint enforcement for `create_derived_memory`
//! (NULL target + non-empty derivation JSON, etc.) is exercised by the
//! bd-2dc25 model-layer slice when it extends `CreateCurationCandidateInput`
//! to carry the new fields. This file pins only what the public API can
//! reach today so V062 ships independently of the model rework.
//!
//! Verification: cargo test --test curation_candidates_v062_unit must be
//! routed through RCH per AGENTS.md. Static rustfmt is supplemental.

#![forbid(unsafe_code)]

use ee::db::{CreateCurationCandidateInput, CreateWorkspaceInput, DbConnection};

type TestResult = Result<(), String>;

const WORKSPACE_ID: &str = "wsp_curation_candidates_v062_unit";
const CREATED_AT: &str = "2026-05-23T07:00:00Z";

fn migrated_connection() -> Result<DbConnection, String> {
    let connection =
        DbConnection::open_memory().map_err(|error| format!("open in-memory db: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate in-memory db: {error}"))?;
    connection
        .insert_workspace(
            WORKSPACE_ID,
            &CreateWorkspaceInput {
                path: "/tmp/ee-curation-candidates-v062-unit".to_owned(),
                name: Some("curation-candidates-v062-unit".to_owned()),
            },
        )
        .map_err(|error| format!("insert workspace: {error}"))?;
    Ok(connection)
}

#[test]
fn v062_migration_applies_cleanly_on_fresh_database() -> TestResult {
    let _ = migrated_connection()?;
    Ok(())
}

#[test]
fn v062_retains_v060_table_for_migration_evidence() -> TestResult {
    let connection = migrated_connection()?;
    let tables = connection
        .list_user_tables()
        .map_err(|error| format!("list user tables: {error}"))?;
    if !tables.iter().any(|name| name == "curation_candidates_v060") {
        return Err(format!(
            "V062 must retain curation_candidates_v060 as migration evidence, got tables {tables:?}"
        ));
    }
    if !tables.iter().any(|name| name == "curation_candidates") {
        return Err("curation_candidates must still exist as the live table".into());
    }
    Ok(())
}

#[test]
fn v062_keeps_v059_and_v060_alongside_live_curation_candidates() -> TestResult {
    let connection = migrated_connection()?;
    let tables = connection
        .list_user_tables()
        .map_err(|error| format!("list user tables: {error}"))?;
    for retained in [
        "curation_candidates_v029",
        "curation_candidates_v033",
        "curation_candidates_v059",
        "curation_candidates_v060",
    ] {
        if !tables.iter().any(|name| name == retained) {
            return Err(format!(
                "expected retained migration table {retained} in {tables:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn v062_preserves_insert_curation_candidate_api_for_ordinary_types() -> TestResult {
    // Regression: V060's `insert_curation_candidate` API must still accept a
    // standard target-mutating candidate after V062 renames the table again.
    // bd-2dc25 will add an Option<String> shape for create_derived_memory; until
    // then, the existing typed input must round-trip exactly as before.
    let connection = migrated_connection()?;
    // Insert a minimal target memory by hand via the workspace bootstrap state;
    // the existing insert API expects target_memory_id to be a valid foreign
    // key, but the schema also accepts a no-existing-row insert because the FK
    // is enforced via `REFERENCES memories(id) ON DELETE CASCADE` which still
    // requires the row to exist on INSERT. So we have to thread a memory in
    // first. Use the same path the production code uses by going through the
    // `execute_raw` helper for a single fixture row.
    connection
        .execute_raw(
            "INSERT INTO memories (id, workspace_id, level, kind, content, confidence, utility, importance, provenance_chain_hash_version, provenance_verification_status, trust_class, valid_from, created_at, updated_at) \
             VALUES ('mem_v062_regression_target_xxxxx', 'wsp_curation_candidates_v062_unit', 'episodic', 'fact', 'v062-regression', 0.9, 0.5, 0.5, 1, 'pending', 'agent_assertion', '2026-05-23T07:00:00Z', '2026-05-23T07:00:00Z', '2026-05-23T07:00:00Z')",
        )
        .map_err(|error| format!("seed target memory: {error}"))?;

    let input = CreateCurationCandidateInput {
        workspace_id: WORKSPACE_ID.to_owned(),
        candidate_type: "promote".to_owned(),
        target_memory_id: "mem_v062_regression_target_xxxxx".to_owned(),
        proposed_content: None,
        proposed_confidence: None,
        proposed_trust_class: None,
        source_type: "agent_inference".to_owned(),
        source_id: None,
        reason: "v062 regression: promote candidate still accepted".to_owned(),
        confidence: 0.7,
        status: None,
        created_at: Some(CREATED_AT.to_owned()),
        ttl_expires_at: None,
    };
    connection
        .insert_curation_candidate("curate_v062_regression_promote_00", &input)
        .map_err(|error| format!("insert promote candidate after V062: {error}"))?;

    let row_count = connection
        .count_table_rows("curation_candidates")
        .map_err(|error| format!("count curation_candidates rows: {error}"))?;
    if row_count != 1 {
        return Err(format!(
            "expected exactly one inserted curation_candidates row, got {row_count}"
        ));
    }
    Ok(())
}

#[test]
fn v062_migration_ledger_includes_create_derived_curation_candidates_entry() -> TestResult {
    let connection = migrated_connection()?;
    let applied = connection
        .applied_migrations()
        .map_err(|error| format!("read applied migrations: {error}"))?;
    if !applied
        .iter()
        .any(|row| row.name() == "create_derived_curation_candidates")
    {
        let names: Vec<&str> = applied.iter().map(|row| row.name()).collect();
        return Err(format!(
            "V062 migration ledger row missing; recorded names: {names:?}"
        ));
    }
    Ok(())
}
