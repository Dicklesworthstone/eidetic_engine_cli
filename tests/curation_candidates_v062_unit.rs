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
        target_memory_id: Some("mem_v062_regression_target_xxxxx".to_owned()),
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
        derivation_source_refs_json: None,
        derivation_metadata_json: None,
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
fn v062_insert_round_trips_create_derived_candidate_without_target() -> TestResult {
    let connection = migrated_connection()?;
    let source_refs_json = format!(
        r#"[{{"kind":"memory","id":"mem_v062_source","contentHash":"blake3:{}"}}]"#,
        "a".repeat(64)
    );
    let metadata_json = serde_json::json!({
        "memorySpec": {
            "level": "procedural",
            "kind": "rule",
            "tags": ["v062"],
            "confidence": 0.7,
            "utility": 0.5,
            "importance": 0.5,
            "validFrom": "2026-05-23T07:00:00Z",
            "validTo": null
        },
        "producer": {
            "producer": "curation_v062_unit",
            "producerPayload": {
                "candidateKind": "regression"
            }
        }
    })
    .to_string();
    let input = CreateCurationCandidateInput {
        workspace_id: WORKSPACE_ID.to_owned(),
        candidate_type: "create_derived_memory".to_owned(),
        target_memory_id: None,
        proposed_content: Some("Derived release rule from v062 regression test.".to_owned()),
        proposed_confidence: Some(0.7),
        proposed_trust_class: Some("agent_assertion".to_owned()),
        source_type: "agent_inference".to_owned(),
        source_id: Some("reflection_v062_unit".to_owned()),
        reason: "v062 regression: create-derived candidate accepted".to_owned(),
        confidence: 0.7,
        status: None,
        created_at: Some(CREATED_AT.to_owned()),
        ttl_expires_at: None,
        derivation_source_refs_json: Some(source_refs_json.clone()),
        derivation_metadata_json: Some(metadata_json.clone()),
    };

    connection
        .insert_curation_candidate("curate_v062_create_derived_000000", &input)
        .map_err(|error| format!("insert create-derived candidate after V062: {error}"))?;

    let stored = connection
        .get_curation_candidate(WORKSPACE_ID, "curate_v062_create_derived_000000")
        .map_err(|error| format!("get create-derived candidate after V062: {error}"))?
        .ok_or_else(|| "create-derived candidate was not persisted".to_owned())?;
    if stored.target_memory_id.is_some() {
        return Err(format!(
            "create-derived candidate target_memory_id must be NULL, got {:?}",
            stored.target_memory_id
        ));
    }
    if stored.derivation_source_refs_json.as_deref() != Some(source_refs_json.as_str()) {
        return Err(format!(
            "source refs did not round-trip: {:?}",
            stored.derivation_source_refs_json
        ));
    }
    if stored.derivation_metadata_json.as_deref() != Some(metadata_json.as_str()) {
        return Err(format!(
            "metadata did not round-trip: {:?}",
            stored.derivation_metadata_json
        ));
    }

    Ok(())
}

#[test]
fn v062_insert_rejects_malformed_create_derived_source_package_before_storage() -> TestResult {
    let connection = migrated_connection()?;
    let invalid = CreateCurationCandidateInput {
        workspace_id: WORKSPACE_ID.to_owned(),
        candidate_type: "create_derived_memory".to_owned(),
        target_memory_id: None,
        proposed_content: Some("Malformed package must not be stored.".to_owned()),
        proposed_confidence: Some(0.7),
        proposed_trust_class: Some("agent_assertion".to_owned()),
        source_type: "agent_inference".to_owned(),
        source_id: Some("reflection_v062_unit".to_owned()),
        reason: "v062 regression: malformed source package rejected".to_owned(),
        confidence: 0.7,
        status: None,
        created_at: Some(CREATED_AT.to_owned()),
        ttl_expires_at: None,
        derivation_source_refs_json: Some(r#"[{"kind":"memory","id":"mem_bad"}]"#.to_owned()),
        derivation_metadata_json: Some(
            serde_json::json!({
                "memorySpec": {"level": "procedural", "kind": "rule"},
                "producer": {"producer": "curation_v062_unit"}
            })
            .to_string(),
        ),
    };

    let before = connection
        .count_table_rows("curation_candidates")
        .map_err(|error| format!("count rows before malformed insert: {error}"))?;
    if connection
        .insert_curation_candidate("curate_v062_bad_source_pkg_000000", &invalid)
        .is_ok()
    {
        return Err("malformed create-derived source package was accepted".to_owned());
    }
    let after = connection
        .count_table_rows("curation_candidates")
        .map_err(|error| format!("count rows after malformed insert: {error}"))?;
    if before != after {
        return Err(format!(
            "malformed insert changed storage row count: before={before} after={after}"
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
