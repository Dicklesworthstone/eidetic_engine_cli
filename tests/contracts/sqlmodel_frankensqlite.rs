//! Gate 2: SQLModel Plus FrankenSQLite Contract Test.
//!
//! Verifies the storage contract before feature code:
//! - Open a temporary FrankenSQLite database through sqlmodel-frankensqlite
//! - Run migrations
//! - Insert and fetch a memory row
//! - Verify migration idempotency
//! - Verify the test uses no rusqlite or SQLx path
//!
//! This test imports from the ee crate to exercise the actual storage layer.

use ee::db::{
    CreateAgentHistorySourceInput, CreateAgentInstallationInput, CreateMemoryInput,
    CreateWorkspaceInput, DbConnection,
};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T: std::fmt::Debug + PartialEq>(actual: &T, expected: &T, ctx: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
    }
}

const TEST_WORKSPACE_ID: &str = "wsp_01234567890123456789012345";
const TEST_MEMORY_ID: &str = "mem_00000000000000000000000001";
const TEST_MEMORY_ID_2: &str = "mem_00000000000000000000000002";
const TEST_AGENT_INSTALLATION_ID: &str = "agi_01234567890123456789012345";
const TEST_AGENT_HISTORY_SOURCE_ID: &str = "ahs_01234567890123456789012345";

fn open_test_db() -> Result<DbConnection, String> {
    DbConnection::open_memory().map_err(|e| e.to_string())
}

fn setup_workspace(conn: &DbConnection) -> TestResult {
    conn.insert_workspace(
        TEST_WORKSPACE_ID,
        &CreateWorkspaceInput {
            path: "/tmp/ee-gate2-test".to_string(),
            name: Some("gate2 test workspace".to_string()),
        },
    )
    .map_err(|e| e.to_string())
}

fn test_memory_input() -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: TEST_WORKSPACE_ID.to_string(),
        level: "working".to_string(),
        kind: "note".to_string(),
        content: "Gate 2 test memory content".to_string(),
        confidence: 0.9,
        utility: 0.7,
        importance: 0.5,
        provenance_uri: None,
        trust_class: "agent_assertion".to_string(),
        trust_subclass: None,
        valid_from: None,
        valid_to: None,
        tags: vec![],
    }
}

#[test]
fn opens_memory_database_through_sqlmodel_frankensqlite() -> TestResult {
    let conn = open_test_db()?;

    ensure_equal(&conn.path(), &":memory:", "memory database path")?;
    conn.ping().map_err(|e| e.to_string())?;
    conn.close().map_err(|e| e.to_string())
}

#[test]
fn runs_migrations_successfully() -> TestResult {
    let conn = open_test_db()?;

    let result = conn.migrate().map_err(|e| e.to_string())?;

    ensure(
        !result.applied().is_empty(),
        "migrations must be applied to fresh database",
    )?;
    ensure_equal(
        &result.skipped().len(),
        &0,
        "no migrations should be skipped",
    )?;

    conn.close().map_err(|e| e.to_string())
}

#[test]
fn inserts_and_fetches_memory_row() -> TestResult {
    let conn = open_test_db()?;
    conn.migrate().map_err(|e| e.to_string())?;
    setup_workspace(&conn)?;

    conn.insert_memory(TEST_MEMORY_ID, &test_memory_input())
        .map_err(|e| e.to_string())?;

    let fetched = conn.get_memory(TEST_MEMORY_ID).map_err(|e| e.to_string())?;

    let memory = fetched.ok_or_else(|| "inserted memory must be fetchable".to_string())?;
    ensure_equal(&memory.id, &TEST_MEMORY_ID.to_string(), "memory id")?;
    ensure_equal(
        &memory.content,
        &"Gate 2 test memory content".to_string(),
        "memory content",
    )?;

    conn.close().map_err(|e| e.to_string())
}

#[test]
fn tombstones_memory_row() -> TestResult {
    let conn = open_test_db()?;
    conn.migrate().map_err(|e| e.to_string())?;
    setup_workspace(&conn)?;

    conn.insert_memory(TEST_MEMORY_ID_2, &test_memory_input())
        .map_err(|e| e.to_string())?;

    let tombstoned = conn
        .tombstone_memory(TEST_MEMORY_ID_2)
        .map_err(|e| e.to_string())?;

    ensure(
        tombstoned,
        "tombstone should return true for existing memory",
    )?;

    let fetched = conn
        .get_memory(TEST_MEMORY_ID_2)
        .map_err(|e| e.to_string())?
        .ok_or("memory should still exist after tombstone")?;

    ensure(
        fetched.tombstoned_at.is_some(),
        "tombstoned memory should have tombstoned_at set",
    )?;

    conn.close().map_err(|e| e.to_string())
}

#[test]
fn migration_is_idempotent() -> TestResult {
    let conn = open_test_db()?;

    let first = conn.migrate().map_err(|e| e.to_string())?;
    let second = conn.migrate().map_err(|e| e.to_string())?;

    ensure(
        !first.applied().is_empty(),
        "first migration should apply changes",
    )?;
    ensure(
        second.applied().is_empty(),
        "second migration should have nothing to apply",
    )?;

    conn.close().map_err(|e| e.to_string())
}

#[test]
fn database_path_is_memory_not_file() -> TestResult {
    let conn = open_test_db()?;

    ensure_equal(&conn.path(), &":memory:", "database path must be :memory:")
}

#[test]
fn ping_succeeds_on_valid_connection() -> TestResult {
    let conn = open_test_db()?;

    conn.ping().map_err(|e| e.to_string())?;
    conn.close().map_err(|e| e.to_string())
}

#[test]
fn close_succeeds_on_valid_connection() -> TestResult {
    let conn = open_test_db()?;
    conn.migrate().map_err(|e| e.to_string())?;

    conn.close().map_err(|e| e.to_string())
}

#[test]
fn memory_tags_can_be_added_and_fetched() -> TestResult {
    let conn = open_test_db()?;
    conn.migrate().map_err(|e| e.to_string())?;
    setup_workspace(&conn)?;

    conn.insert_memory(TEST_MEMORY_ID, &test_memory_input())
        .map_err(|e| e.to_string())?;

    conn.add_memory_tags(TEST_MEMORY_ID, &["test".to_string(), "gate2".to_string()])
        .map_err(|e| e.to_string())?;

    let tags = conn
        .get_memory_tags(TEST_MEMORY_ID)
        .map_err(|e| e.to_string())?;

    ensure_equal(&tags.len(), &2, "should have 2 tags")?;
    ensure(
        tags.contains(&"test".to_string()),
        "should contain 'test' tag",
    )?;
    ensure(
        tags.contains(&"gate2".to_string()),
        "should contain 'gate2' tag",
    )?;

    conn.close().map_err(|e| e.to_string())
}

#[test]
fn agent_detection_repositories_persist_installations_and_sources() -> TestResult {
    let conn = open_test_db()?;
    conn.migrate().map_err(|e| e.to_string())?;
    setup_workspace(&conn)?;

    conn.upsert_agent_installation(
        TEST_AGENT_INSTALLATION_ID,
        &CreateAgentInstallationInput {
            workspace_id: TEST_WORKSPACE_ID.to_string(),
            slug: "codex".to_string(),
            detected: true,
            detection_format_version: 1,
            evidence: vec!["root_exists".to_string()],
            root_paths: vec!["/home/test/.codex/sessions".to_string()],
            observed_at: "2026-01-01T00:00:00Z".to_string(),
            metadata_json: Some(r#"{"source":"contract"}"#.to_string()),
        },
    )
    .map_err(|e| e.to_string())?;

    conn.upsert_agent_history_source(
        TEST_AGENT_HISTORY_SOURCE_ID,
        &CreateAgentHistorySourceInput {
            workspace_id: TEST_WORKSPACE_ID.to_string(),
            installation_id: Some(TEST_AGENT_INSTALLATION_ID.to_string()),
            agent_slug: "codex".to_string(),
            source_kind: "probe_path".to_string(),
            source_path: "~/.codex/sessions".to_string(),
            path_exists: true,
            observed_at: "2026-01-01T00:00:00Z".to_string(),
            metadata_json: None,
        },
    )
    .map_err(|e| e.to_string())?;

    let installations = conn
        .list_agent_installations(TEST_WORKSPACE_ID)
        .map_err(|e| e.to_string())?;
    ensure_equal(&installations.len(), &1, "installation count")?;
    let installation = installations
        .first()
        .ok_or_else(|| "installation should exist after count check".to_string())?;
    ensure_equal(&installation.slug, &"codex".to_string(), "slug")?;
    ensure(installation.detected, "installation detected")?;

    let sources = conn
        .list_agent_history_sources_for_agent(TEST_WORKSPACE_ID, "codex")
        .map_err(|e| e.to_string())?;
    ensure_equal(&sources.len(), &1, "history source count")?;
    let source = sources
        .first()
        .ok_or_else(|| "history source should exist after count check".to_string())?;
    ensure_equal(
        &source.source_path,
        &"~/.codex/sessions".to_string(),
        "source path",
    )?;
    ensure(source.path_exists, "source exists")?;

    conn.close().map_err(|e| e.to_string())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn ensure_helper_passes_on_true() {
        assert!(ensure(true, "should pass").is_ok());
    }

    #[test]
    fn ensure_helper_fails_on_false() {
        assert!(ensure(false, "should fail").is_err());
    }

    #[test]
    fn ensure_equal_passes_on_match() {
        assert!(ensure_equal(&42, &42, "test").is_ok());
    }

    #[test]
    fn ensure_equal_fails_on_mismatch() {
        assert!(ensure_equal(&42, &43, "test").is_err());
    }
}
