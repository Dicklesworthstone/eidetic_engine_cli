//! bd-u875s.3 — golden + contract tests for the `ee recall` CLI surface.
//!
//! The fixture seeds a fresh workspace DB with FIXED memory ids and explicit
//! `anchor:path:…` / `anchor:symbol:…` tokens through the library API, then
//! runs the real binary, so output is byte-deterministic across machines:
//! `ee.recall.v1` carries no wall-clock timestamps, no workspace paths, and
//! no binary version. Goldens cover both formats; the contract test
//! validates the JSON payload structurally against
//! `docs/schemas/ee.recall.v1.json` (required sets, enums, const fields).
//! Budget paging is asserted to partition the ranked set exactly once
//! (no duplicates, no gaps) and rejected cursors must yield an EMPTY page
//! with the ADR 0063 cursor vocabulary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

use ee::core::recall::{RecallCandidateRow, RecallProvenanceRef, RecallQuery, evaluate_recall};
use ee::models::{MemoryAnchorFreshnessState, MemoryAnchorKind};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use serde_json::Value;

type TestResult = Result<(), String>;

const FIXTURE_WORKSPACE_ID: &str = "wsp_00000000000000000000000081";
const EMPTY_WORKSPACE_ID: &str = "wsp_00000000000000000000000082";

const MEM_RULE: &str = "mem_00000000000000000000000041";
const MEM_FAILURE: &str = "mem_00000000000000000000000042";
const MEM_DECISION: &str = "mem_00000000000000000000000043";

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_MAX_OUTPUT_TOKENS")
        .env("EE_EMBED_DOWNLOAD", "off")
        .output()
        .map_err(|error| format!("failed to run ee {args:?}: {error}"))
}

fn seed_workspace(
    workspace_id: &str,
    seeds: &[(&str, &str, &str, &str, f32)],
) -> Result<tempfile::TempDir, String> {
    seed_workspace_at_schema(workspace_id, seeds, None)
}

fn seed_workspace_at_schema(
    workspace_id: &str,
    seeds: &[(&str, &str, &str, &str, f32)],
    schema_version: Option<u32>,
) -> Result<tempfile::TempDir, String> {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let db_dir = temp.path().join(".ee");
    std::fs::create_dir_all(&db_dir).map_err(|error| format!("mkdir .ee: {error}"))?;
    let database = db_dir.join("ee.db");
    if let Some(version) = schema_version {
        // Build the historical schema itself, not a current database with its
        // version marker rewritten. These are the published migration bodies
        // and computed checksums, stopped before the new source table exists.
        // Fresh connections avoid retaining roots retired by table rebuilds.
        for migration in ee::db::MIGRATIONS
            .iter()
            .filter(|migration| migration.version() <= version)
        {
            let connection =
                ee::db::DbConnection::open_file(&database).map_err(|error| error.to_string())?;
            connection
                .ensure_migration_table()
                .map_err(|error| error.to_string())?;
            // The fixture is still empty. Official table replacements need
            // the same foreign-key relaxation as the migration runner.
            connection
                .execute_raw("PRAGMA foreign_keys = OFF")
                .map_err(|error| error.to_string())?;
            let record = ee::db::MigrationRecord::new(
                migration.version(),
                migration.name(),
                migration.checksum(),
                "2026-10-22T00:00:00Z",
            )
            .map_err(|error| error.to_string())?;
            connection
                .with_transaction(|| {
                    connection.execute_raw(migration.sql())?;
                    connection.record_migration(&record)
                })
                .map_err(|error| format!("legacy migration V{}: {error}", migration.version()))?;
            connection.close().map_err(|error| error.to_string())?;
        }
    }
    let connection =
        ee::db::DbConnection::open_file(&database).map_err(|error| format!("open db: {error}"))?;
    if let Some(version) = schema_version {
        assert_eq!(
            connection
                .schema_version()
                .map_err(|error| error.to_string())?,
            Some(version)
        );
        connection
            .validate_applied_migrations()
            .map_err(|error| error.to_string())?;
        assert!(
            connection
                .check_foreign_keys()
                .map_err(|error| error.to_string())?
                .passed
        );
    } else {
        connection
            .migrate()
            .map_err(|error| format!("migrate: {error}"))?;
    }
    connection
        .insert_workspace(
            workspace_id,
            &ee::db::CreateWorkspaceInput {
                path: temp.path().to_string_lossy().into_owned(),
                name: Some("recall-golden".to_owned()),
            },
        )
        .map_err(|error| format!("insert workspace: {error}"))?;
    for (id, level, kind, content, confidence) in seeds {
        connection
            .insert_memory(
                id,
                &ee::db::CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: (*level).to_owned(),
                    kind: (*kind).to_owned(),
                    content: (*content).to_owned(),
                    workflow_id: None,
                    confidence: *confidence,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("test://recall-golden".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["recall-golden".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| format!("insert memory {id}: {error}"))?;
    }
    connection
        .close()
        .map_err(|error| format!("close db: {error}"))?;
    Ok(temp)
}

/// Three memories with explicit anchors: a procedural rule and an episodic
/// failure both anchored on `src/db/mod.rs` (the failure also carries a
/// symbol anchor), plus a semantic decision anchored elsewhere so glob
/// scoping is observable.
fn seed_recall_workspace() -> Result<tempfile::TempDir, String> {
    seed_workspace(
        FIXTURE_WORKSPACE_ID,
        &[
            (
                MEM_RULE,
                "procedural",
                "rule",
                "Always route Cargo verification through RCH for storage-layer changes. \
                 anchor:path:src/db/mod.rs",
                0.9,
            ),
            (
                MEM_FAILURE,
                "episodic",
                "failure",
                "Linker OOM at default -j when rebuilding the DB layer. \
                 anchor:path:src/db/mod.rs anchor:symbol:DbConnection",
                0.8,
            ),
            (
                MEM_DECISION,
                "semantic",
                "decision",
                "Graph snapshots persist centrality scores for the primer. \
                 anchor:path:src/graph/mod.rs",
                0.85,
            ),
        ],
    )
}

fn durable_recall_files(
    directory: &std::path::Path,
) -> Result<std::collections::BTreeMap<PathBuf, Vec<u8>>, String> {
    fn visit(
        path: &std::path::Path,
        files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) -> TestResult {
        for entry in std::fs::read_dir(path).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() {
                visit(&entry.path(), files)?;
            } else if kind.is_file() && !entry.file_name().to_string_lossy().ends_with("-shm") {
                // Reader coordination is transient. WAL, source DB, audits,
                // writer-owner epochs, and every other durable file are not.
                files.insert(
                    entry.path(),
                    std::fs::read(entry.path()).map_err(|error| error.to_string())?,
                );
            }
        }
        Ok(())
    }
    let mut files = std::collections::BTreeMap::new();
    visit(directory, &mut files)?;
    Ok(files)
}

#[cfg(unix)]
#[test]
fn recall_read_only_remains_available_under_a_real_writer_fence() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let database = workspace.path().join(".ee/ee.db");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(database.with_extension("write.lock"))
        .map_err(|error| error.to_string())?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|error| error.to_string())?;
    let before = durable_recall_files(workspace.path())?;
    let workspace_arg = workspace.path().to_str().ok_or("workspace path")?;
    for selectors in [
        vec!["--path", "src/db/mod.rs"],
        vec!["--symbol", "DbConnection"],
        vec!["--path", "src/**", "--budget-tokens", "20"],
    ] {
        let mut args = vec!["recall", "--workspace", workspace_arg, "--json"];
        args.extend(selectors);
        let response = parse_response(&run_ee(&args)?, "read-only recall while writer is held")?;
        assert!(!item_memory_ids(&response).is_empty());
        assert_eq!(durable_recall_files(workspace.path())?, before);
    }
    let rejected = parse_response(
        &run_ee(&[
            "recall",
            "--workspace",
            workspace_arg,
            "--json",
            "--path",
            "src/**",
            "--cursor",
            "invalid-cursor",
        ])?,
        "read-only cursor rejection",
    )?;
    assert!(item_memory_ids(&rejected).is_empty());
    assert!(
        degraded_codes(&rejected)
            .iter()
            .any(|code| code == "cursor_invalid")
    );
    assert_eq!(durable_recall_files(workspace.path())?, before);
    drop(lock);
    Ok(())
}

#[cfg(unix)]
#[test]
fn recall_read_only_accepts_nonwritable_database_without_changing_answers() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let workspace = seed_recall_workspace()?;
    let database = workspace.path().join(".ee/ee.db");
    let workspace_arg = workspace.path().to_str().ok_or("workspace path")?;
    let args = [
        "recall",
        "--workspace",
        workspace_arg,
        "--json",
        "--path",
        "src/**",
    ];
    let expected = parse_response(&run_ee(&args)?, "unrestricted recall")?;
    assert_eq!(item_memory_ids(&expected).len(), 3);
    let before = durable_recall_files(workspace.path())?;
    let permissions = std::fs::metadata(&database)
        .map_err(|error| error.to_string())?
        .permissions();
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o444))
        .map_err(|error| error.to_string())?;
    let result = run_ee(&args);
    std::fs::set_permissions(&database, permissions).map_err(|error| error.to_string())?;
    let response = parse_response(&result?, "nonwritable recall")?;
    assert_eq!(response, expected);
    assert_eq!(durable_recall_files(workspace.path())?, before);
    Ok(())
}

#[test]
fn recall_read_only_refuses_implicit_migration_and_missing_store_creation() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let database = workspace.path().join(".ee/ee.db");
    let db = ee::db::DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    db.execute_raw("DELETE FROM ee_schema_migrations WHERE version = (SELECT MAX(version) FROM ee_schema_migrations)")
        .map_err(|error| error.to_string())?;
    assert!(db.needs_migration().map_err(|error| error.to_string())?);
    db.close().map_err(|error| error.to_string())?;
    let before = durable_recall_files(workspace.path())?;
    let output = run_ee(&[
        "recall",
        "--workspace",
        workspace.path().to_str().ok_or("workspace path")?,
        "--json",
        "--path",
        "src/**",
    ])?;
    assert_eq!(output.status.code(), Some(8));
    let response: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    assert_eq!(response["schema"], "ee.error.v2");
    assert_eq!(response["error"]["code"], "migration_required");
    assert!(
        output
            .stdout
            .windows(b"ee migrate run".len())
            .any(|window| window == b"ee migrate run")
    );
    assert_eq!(durable_recall_files(workspace.path())?, before);
    let empty = tempfile::tempdir().map_err(|error| error.to_string())?;
    let missing = run_ee(&[
        "recall",
        "--workspace",
        empty.path().to_str().ok_or("workspace path")?,
        "--json",
        "--path",
        "src/**",
    ])?;
    assert!(!missing.status.success());
    let response: Value =
        serde_json::from_slice(&missing.stdout).map_err(|error| error.to_string())?;
    assert_eq!(response["error"]["code"], "workspace_store_missing");
    assert!(!empty.path().join(".ee").exists());
    Ok(())
}

/// Self-contained golden compare/update (same UPDATE_GOLDEN contract as
/// tests/golden.rs, without pulling that file's test module into this
/// binary).
fn assert_recall_golden(file_name: &str, actual: &str) -> TestResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("recall")
        .join(file_name);
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        std::fs::write(&path, actual)
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        return Ok(());
    }
    let expected = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "read {}: {error} (run with UPDATE_GOLDEN=1)",
            path.display()
        )
    })?;
    if expected == actual {
        Ok(())
    } else {
        Err(format!(
            "golden mismatch for {}.\n--- expected\n{expected}\n+++ actual\n{actual}",
            path.display()
        ))
    }
}

fn parse_response(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!(
            "{context} failed: {}\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_str(stdout.trim()).map_err(|error| format!("{context}: parse: {error}"))
}

fn degraded_codes(response: &Value) -> Vec<String> {
    response
        .pointer("/degraded")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.pointer("/code").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn item_memory_ids(response: &Value) -> Vec<String> {
    response
        .pointer("/data/recall/items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.pointer("/memoryId").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn anchor_cli(workspace: &std::path::Path, args: &[&str]) -> Result<Value, String> {
    let mut command = vec![
        "--workspace",
        workspace.to_str().ok_or("workspace path")?,
        "--json",
    ];
    command.extend_from_slice(args);
    parse_response(&run_ee(&command)?, &args.join(" "))
}

/// Source identity is deliberately captured separately from anchor metadata,
/// audits, and derived indexes: attaching a locator must not revise the memory
/// or rewrite the evidence that justified it.
#[derive(Debug, PartialEq)]
struct AnchorSourceSnapshot {
    memory: ee::db::StoredMemory,
    logical_id: Option<String>,
    superseded_at: Option<String>,
    tags: Vec<String>,
    typed_fields: Option<String>,
    evidence: Vec<ee::db::StoredEvidenceSpan>,
}

fn anchor_source_snapshot(
    workspace: &std::path::Path,
    memory_id: &str,
) -> Result<AnchorSourceSnapshot, String> {
    let db = ee::db::DbConnection::open_file_read_only(workspace.join(".ee/ee.db"))
        .map_err(|error| error.to_string())?;
    let snapshot = AnchorSourceSnapshot {
        memory: db
            .get_memory(memory_id)
            .map_err(|error| error.to_string())?
            .ok_or("memory missing")?,
        logical_id: db
            .get_memory_logical_id(memory_id)
            .map_err(|error| error.to_string())?,
        superseded_at: db
            .get_memory_superseded_at(memory_id)
            .map_err(|error| error.to_string())?,
        tags: db
            .get_memory_tags(memory_id)
            .map_err(|error| error.to_string())?,
        typed_fields: db
            .get_memory_typed_fields_json(memory_id)
            .map_err(|error| error.to_string())?,
        evidence: db
            .list_evidence_spans_for_memory(memory_id)
            .map_err(|error| error.to_string())?,
    };
    db.close().map_err(|error| error.to_string())?;
    Ok(snapshot)
}

fn assert_path_recalls(
    workspace: &std::path::Path,
    path: &str,
    expected_ids: &[&str],
) -> TestResult {
    let response = anchor_cli(workspace, &["recall", "--path", path])?;
    assert_eq!(
        item_memory_ids(&response),
        expected_ids
            .iter()
            .map(|id| (*id).to_owned())
            .collect::<Vec<_>>(),
        "path recall must return the requested durable memory: {response}"
    );
    Ok(())
}

#[test]
fn code_anchor_creation_through_documented_inline_tokens_has_positive_recall() -> TestResult {
    let workspace = seed_workspace(FIXTURE_WORKSPACE_ID, &[])?;
    let content = "Storage changes require bounded transactions. \
        ee-anchor:path:src/db/mod.rs ee-anchor:symbol:crate::DbConnection \
        ee-anchor:path:.github/workflows/test.yml anchor:path:app/controller.py";
    let remembered = anchor_cli(
        workspace.path(),
        &[
            "remember",
            content,
            "--level",
            "semantic",
            "--kind",
            "fact",
            "--source",
            "manual://code-anchor-test",
            "--no-propose-candidates",
        ],
    )?;
    let memory_id = remembered["data"]["memoryId"]
        .as_str()
        .ok_or("remember must return memoryId")?;
    let source = anchor_source_snapshot(workspace.path(), memory_id)?;
    assert_eq!(source.memory.content, content);
    assert_eq!(
        source.memory.provenance_uri.as_deref(),
        Some("manual://code-anchor-test")
    );
    for rebuild in [false, true] {
        if rebuild {
            anchor_cli(workspace.path(), &["index", "rebuild"])?;
        }
        for path in [
            "src/db/mod.rs",
            ".github/workflows/test.yml",
            "app/controller.py",
        ] {
            assert_path_recalls(workspace.path(), path, &[memory_id])?;
        }
        let symbols = anchor_cli(
            workspace.path(),
            &["recall", "--symbol", "crate::DbConnection"],
        )?;
        assert_eq!(item_memory_ids(&symbols), vec![memory_id.to_owned()]);
    }
    assert_eq!(anchor_source_snapshot(workspace.path(), memory_id)?, source);
    Ok(())
}

#[test]
fn code_anchor_schema131_previews_do_not_migrate_and_both_apply_routes_upgrade() -> TestResult {
    for backfill_first in [false, true] {
        let workspace = seed_workspace_at_schema(
            FIXTURE_WORKSPACE_ID,
            &[
                (
                    MEM_RULE,
                    "semantic",
                    "fact",
                    "Connection tuning uses `db.cache_size`.",
                    0.9,
                ),
                (
                    MEM_FAILURE,
                    "episodic",
                    "failure",
                    "Review src/legacy.rs before retrying the failed transaction.",
                    0.8,
                ),
            ],
            Some(131),
        )?;
        let database = workspace.path().join(".ee/ee.db");
        let db = ee::db::DbConnection::open_file_read_only(&database)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            db.schema_version().map_err(|error| error.to_string())?,
            Some(131)
        );
        assert!(
            !db.list_user_tables()
                .map_err(|error| error.to_string())?
                .iter()
                .any(|table| table == "memory_code_anchor_sources")
        );
        let migrations = db.applied_migrations().map_err(|error| error.to_string())?;
        db.close().map_err(|error| error.to_string())?;
        let originals = [
            anchor_source_snapshot(workspace.path(), MEM_RULE)?,
            anchor_source_snapshot(workspace.path(), MEM_FAILURE)?,
        ];
        let files_before = durable_recall_files(workspace.path())?;

        let listing = anchor_cli(workspace.path(), &["memory", "anchors", MEM_RULE])?;
        assert_eq!(listing["data"]["status"], "listed");
        assert_eq!(listing["data"]["persisted"], false);
        let attachment = anchor_cli(
            workspace.path(),
            &[
                "memory",
                "anchors",
                MEM_RULE,
                "--path",
                "src/upgraded.rs",
                "--symbol",
                "Legacy::upgrade",
                "--dry-run",
            ],
        )?;
        assert_eq!(attachment["data"]["status"], "would_attach");
        assert_eq!(attachment["data"]["addedAnchorCount"], 2);
        let preview = anchor_cli(workspace.path(), &["index", "backfill-anchors"])?;
        assert_eq!(preview["data"]["dryRun"], true);
        assert_eq!(preview["data"]["proposedMemoryCount"], 1);
        assert_eq!(preview["data"]["proposedAnchorCount"], 1);
        assert_eq!(durable_recall_files(workspace.path())?, files_before);
        let db = ee::db::DbConnection::open_file_read_only(&database)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            db.schema_version().map_err(|error| error.to_string())?,
            Some(131)
        );
        assert_eq!(
            db.applied_migrations().map_err(|error| error.to_string())?,
            migrations
        );
        assert!(
            !db.list_user_tables()
                .map_err(|error| error.to_string())?
                .iter()
                .any(|table| table == "memory_code_anchor_sources")
        );
        db.close().map_err(|error| error.to_string())?;

        // Exercise each independent migration entry point against an actual
        // V131 store, then ensure the other operation remains usable afterward.
        for backfill in [backfill_first, !backfill_first] {
            if backfill {
                let applied =
                    anchor_cli(workspace.path(), &["index", "backfill-anchors", "--apply"])?;
                assert_eq!(applied["data"]["appliedMemoryCount"], 1);
                assert_eq!(applied["data"]["appliedAnchorCount"], 1);
                assert_path_recalls(workspace.path(), "src/legacy.rs", &[MEM_FAILURE])?;
            } else {
                let attached = anchor_cli(
                    workspace.path(),
                    &[
                        "memory",
                        "anchors",
                        MEM_RULE,
                        "--path",
                        "src/upgraded.rs",
                        "--symbol",
                        "Legacy::upgrade",
                    ],
                )?;
                assert_eq!(attached["data"]["status"], "attached");
                assert_eq!(attached["data"]["addedAnchorCount"], 2);
                assert_path_recalls(workspace.path(), "src/upgraded.rs", &[MEM_RULE])?;
            }
            let db = ee::db::DbConnection::open_file_read_only(&database)
                .map_err(|error| error.to_string())?;
            assert_eq!(
                db.schema_version().map_err(|error| error.to_string())?,
                ee::db::MIGRATIONS
                    .last()
                    .map(|migration| migration.version())
            );
            assert!(
                db.list_user_tables()
                    .map_err(|error| error.to_string())?
                    .iter()
                    .any(|table| table == "memory_code_anchor_sources")
            );
            db.close().map_err(|error| error.to_string())?;
        }
        anchor_cli(workspace.path(), &["index", "rebuild"])?;
        assert_path_recalls(workspace.path(), "src/upgraded.rs", &[MEM_RULE])?;
        assert_path_recalls(workspace.path(), "src/legacy.rs", &[MEM_FAILURE])?;
        for (id, original) in [MEM_RULE, MEM_FAILURE].into_iter().zip(originals) {
            assert_eq!(anchor_source_snapshot(workspace.path(), id)?, original);
        }
    }
    Ok(())
}

fn write_anchor_source_files(workspace: &std::path::Path) -> TestResult {
    for relative in [
        "app/controller.py",
        "lib/widget.ts",
        ".github/workflows/test.yml",
        "package.json",
    ] {
        let file = workspace.join(relative);
        std::fs::create_dir_all(file.parent().ok_or("source file parent")?)
            .map_err(|error| error.to_string())?;
        std::fs::write(file, "fixture source\n").map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[test]
fn code_anchor_explicit_paths_cover_repo_layouts_and_missing_historical_files() -> TestResult {
    let workspace = seed_workspace(
        FIXTURE_WORKSPACE_ID,
        &[(
            MEM_RULE,
            "semantic",
            "fact",
            "Keep the original source observation intact.",
            0.9,
        )],
    )?;
    write_anchor_source_files(workspace.path())?;
    assert!(!workspace.path().join("app/deleted.py").exists());
    let original = anchor_source_snapshot(workspace.path(), MEM_RULE)?;
    let attached = anchor_cli(
        workspace.path(),
        &[
            "memory",
            "anchors",
            MEM_RULE,
            "--path",
            "./app/controller.py",
            "--path",
            "lib/widget.ts",
            "--path",
            ".github/workflows/test.yml",
            "--path",
            "package.json",
            "--path",
            "app/deleted.py",
        ],
    )?;
    assert_eq!(attached["data"]["addedAnchorCount"], 5);
    for rebuild in [false, true] {
        if rebuild {
            anchor_cli(workspace.path(), &["index", "rebuild"])?;
        }
        for path in [
            "app/controller.py",
            "lib/widget.ts",
            ".github/workflows/test.yml",
            "package.json",
            "app/deleted.py",
        ] {
            assert_path_recalls(workspace.path(), path, &[MEM_RULE])?;
        }
        assert_eq!(
            anchor_source_snapshot(workspace.path(), MEM_RULE)?,
            original
        );
    }
    Ok(())
}

#[test]
fn code_anchor_backfill_resolves_real_workspace_files_without_inventing_prose_locators()
-> TestResult {
    let workspace = seed_workspace(
        FIXTURE_WORKSPACE_ID,
        &[(
            MEM_RULE,
            "semantic",
            "fact",
            "Review app/controller.py, lib/widget.ts, .github/workflows/test.yml and package.json before the release. Ignore app/unknown.py, imaginary.txt and public.py until verified.",
            0.9,
        )],
    )?;
    write_anchor_source_files(workspace.path())?;
    // An ordinary prose word remains prose even if a same-named file exists.
    std::fs::write(workspace.path().join("release"), "not a code locator\n")
        .map_err(|error| error.to_string())?;
    let outside = tempfile::tempdir().map_err(|error| error.to_string())?;
    std::fs::write(outside.path().join("private.py"), "outside source\n")
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("private.py"),
        workspace.path().join("public.py"),
    )
    .map_err(|error| error.to_string())?;
    let original = anchor_source_snapshot(workspace.path(), MEM_RULE)?;
    let files_before = durable_recall_files(workspace.path())?;
    let preview = anchor_cli(workspace.path(), &["index", "backfill-anchors"])?;
    assert_eq!(preview["data"]["proposedMemoryCount"], 1);
    assert_eq!(preview["data"]["proposedAnchorCount"], 4);
    assert_eq!(durable_recall_files(workspace.path())?, files_before);
    let applied = anchor_cli(workspace.path(), &["index", "backfill-anchors", "--apply"])?;
    assert_eq!(applied["data"]["appliedAnchorCount"], 4);
    let db = ee::db::DbConnection::open_file_read_only(workspace.path().join(".ee/ee.db"))
        .map_err(|error| error.to_string())?;
    let locators = db
        .list_memory_code_anchor_sources(MEM_RULE)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|surface| surface.normalized_value)
        .collect::<std::collections::BTreeSet<_>>();
    let expected = [
        "app/controller.py",
        "lib/widget.ts",
        ".github/workflows/test.yml",
        "package.json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        locators, expected,
        "backfill must not follow escaping links or infer words and missing files"
    );
    db.close().map_err(|error| error.to_string())?;
    for path in &expected {
        assert_path_recalls(workspace.path(), path, &[MEM_RULE])?;
    }
    anchor_cli(workspace.path(), &["index", "rebuild"])?;
    for path in &expected {
        assert_path_recalls(workspace.path(), path, &[MEM_RULE])?;
    }
    assert_eq!(
        anchor_source_snapshot(workspace.path(), MEM_RULE)?,
        original
    );
    let repeated = anchor_cli(workspace.path(), &["index", "backfill-anchors", "--apply"])?;
    assert_eq!(repeated["data"]["appliedAnchorCount"], 0);
    Ok(())
}

#[test]
fn code_anchor_backfill_limit_skips_the_whole_memory_and_preserves_pagination() -> TestResult {
    let body = |count| {
        (0..count)
            .map(|index| format!("src/cap_{index:03}.rs"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let oversized = body(257);
    let at_limit = body(256);
    let workspace = seed_workspace(
        FIXTURE_WORKSPACE_ID,
        &[
            (MEM_RULE, "semantic", "fact", &oversized, 0.9),
            (MEM_FAILURE, "semantic", "fact", &at_limit, 0.9),
        ],
    )?;
    let database = workspace.path().join(".ee/ee.db");
    let state = || -> Result<(i64, i64, i64, Option<u64>), String> {
        let db = ee::db::DbConnection::open_file_read_only(&database)
            .map_err(|error| error.to_string())?;
        let result = (
            db.count_table_rows("memory_code_anchor_sources")
                .map_err(|error| error.to_string())?,
            db.count_table_rows("audit_log")
                .map_err(|error| error.to_string())?,
            db.count_table_rows("search_index_jobs")
                .map_err(|error| error.to_string())?,
            db.get_workspace_generation(FIXTURE_WORKSPACE_ID)
                .map_err(|error| error.to_string())?,
        );
        db.close().map_err(|error| error.to_string())?;
        Ok(result)
    };
    let originals = [
        anchor_source_snapshot(workspace.path(), MEM_RULE)?,
        anchor_source_snapshot(workspace.path(), MEM_FAILURE)?,
    ];
    let state_before = state()?;
    let files_before = durable_recall_files(workspace.path())?;
    for apply in [false, true] {
        let mut args = vec!["index", "backfill-anchors", "--limit", "1"];
        if apply {
            args.push("--apply");
        }
        let skipped = anchor_cli(workspace.path(), &args)?;
        assert_eq!(skipped["data"]["scannedMemoryCount"], 1);
        assert_eq!(skipped["data"]["proposedMemoryCount"], 0);
        assert_eq!(skipped["data"]["appliedAnchorCount"], 0);
        assert_eq!(skipped["data"]["skippedMemoryCount"], 1);
        assert_eq!(skipped["data"]["nextAfter"], MEM_RULE);
        assert_eq!(
            skipped["data"]["skippedMemories"],
            serde_json::json!([
                {"memoryId": MEM_RULE, "reason": "anchor_limit_exceeded", "candidateAnchorCount": 257, "anchorLimit": 256}
            ])
        );
        if !apply {
            assert_eq!(durable_recall_files(workspace.path())?, files_before);
        }
        assert_eq!(
            state()?,
            state_before,
            "skipping must not attach a partial prefix or enqueue writes"
        );
    }
    let preview = anchor_cli(
        workspace.path(),
        &[
            "index",
            "backfill-anchors",
            "--limit",
            "1",
            "--after",
            MEM_RULE,
        ],
    )?;
    assert_eq!(preview["data"]["proposedMemoryCount"], 1);
    assert_eq!(preview["data"]["proposedAnchorCount"], 256);
    assert_eq!(preview["data"]["appliedAnchorCount"], 0);
    assert!(preview["data"]["nextAfter"].is_null());
    assert_eq!(state()?, state_before);
    let applied = anchor_cli(
        workspace.path(),
        &[
            "index",
            "backfill-anchors",
            "--limit",
            "1",
            "--after",
            MEM_RULE,
            "--apply",
        ],
    )?;
    assert_eq!(applied["data"]["appliedMemoryCount"], 1);
    assert_eq!(applied["data"]["appliedAnchorCount"], 256);
    assert_eq!(applied["data"]["skippedMemoryCount"], 0);
    assert_eq!(state()?.0, 256);
    assert_path_recalls(workspace.path(), "src/cap_255.rs", &[MEM_FAILURE])?;
    assert_path_recalls(workspace.path(), "src/cap_256.rs", &[])?;
    for (id, original) in [MEM_RULE, MEM_FAILURE].into_iter().zip(originals) {
        assert_eq!(anchor_source_snapshot(workspace.path(), id)?, original);
    }
    Ok(())
}

#[test]
fn code_anchor_attachment_preserves_source_and_recovers_a_config_only_workspace() -> TestResult {
    let workspace = seed_workspace(
        FIXTURE_WORKSPACE_ID,
        &[(
            MEM_RULE,
            "semantic",
            "fact",
            "Connection tuning uses `db.cache_size`.",
            0.9,
        )],
    )?;
    let db_path = workspace.path().join(".ee/ee.db");
    let db = ee::db::DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
    let session_id = ee::models::SessionId::from_uuid(uuid::Uuid::from_u128(0x8401)).to_string();
    let evidence_id = ee::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(0x8402)).to_string();
    let excerpt = "Observed the existing connection tuning behavior.";
    db.insert_session(
        &session_id,
        &ee::db::CreateSessionInput {
            workspace_id: FIXTURE_WORKSPACE_ID.to_owned(),
            cass_session_id: "anchor-provenance-fixture".to_owned(),
            source_path: None,
            agent_name: Some("fixture".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 1,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
            metadata_json: None,
        },
    )
    .map_err(|error| error.to_string())?;
    db.insert_evidence_span(
        &evidence_id,
        &ee::db::CreateEvidenceSpanInput {
            workspace_id: FIXTURE_WORKSPACE_ID.to_owned(),
            session_id: session_id.clone(),
            memory_id: Some(MEM_RULE.to_owned()),
            producer_kind: ee::db::EvidenceProducerKind::CassImport,
            cass_span_id: "anchor-provenance-span".to_owned(),
            span_kind: "message".to_owned(),
            start_line: 1,
            end_line: 1,
            start_byte: None,
            end_byte: None,
            role: Some("assistant".to_owned()),
            excerpt: excerpt.to_owned(),
            content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
            metadata_json: None,
            inherited_redaction_classes: Vec::new(),
        },
    )
    .map_err(|error| error.to_string())?;
    let session_before = db
        .get_session(&session_id)
        .map_err(|error| error.to_string())?;
    let original_anchors = db
        .list_memory_anchors(MEM_RULE)
        .map_err(|error| error.to_string())?;
    assert!(
        original_anchors
            .iter()
            .any(|anchor| anchor.anchor_kind == MemoryAnchorKind::ConfigKey)
    );
    assert!(original_anchors.iter().all(|anchor| !matches!(
        anchor.anchor_kind,
        MemoryAnchorKind::Path | MemoryAnchorKind::Symbol
    )));
    db.close().map_err(|error| error.to_string())?;

    // This is the reported failure: rebuilding does not invent a code locator
    // for content that only names a configuration key.
    anchor_cli(workspace.path(), &["index", "rebuild"])?;
    let empty = anchor_cli(workspace.path(), &["recall", "--path", "src/db/mod.rs"])?;
    assert!(item_memory_ids(&empty).is_empty());
    let missing = empty["degraded"]
        .as_array()
        .ok_or("degraded array")?
        .iter()
        .find(|entry| entry["code"] == "anchor_index_empty")
        .ok_or("missing anchors must report anchor_index_empty")?;
    assert!(
        missing["repair"]
            .as_str()
            .unwrap_or_default()
            .contains("backfill-anchors")
    );

    let before = anchor_source_snapshot(workspace.path(), MEM_RULE)?;
    assert_eq!(
        before.evidence.len(),
        1,
        "provenance preservation must be non-vacuous"
    );
    let files_before = durable_recall_files(workspace.path())?;
    let preview = anchor_cli(
        workspace.path(),
        &[
            "memory",
            "anchors",
            MEM_RULE,
            "--path",
            "./src/db/mod.rs",
            "--path",
            "src/db/mod.rs",
            "--symbol",
            "crate::DbConnection",
            "--dry-run",
        ],
    )?;
    assert_eq!(preview["data"]["dryRun"], true);
    assert_eq!(preview["data"]["persisted"], false);
    assert_eq!(preview["data"]["addedAnchorCount"], 2);
    assert_eq!(durable_recall_files(workspace.path())?, files_before);
    assert_eq!(anchor_source_snapshot(workspace.path(), MEM_RULE)?, before);
    assert_path_recalls(workspace.path(), "src/db/mod.rs", &[])?;

    let applied = anchor_cli(
        workspace.path(),
        &[
            "memory",
            "anchors",
            MEM_RULE,
            "--path",
            "./src/db/mod.rs",
            "--path",
            "src/db/mod.rs",
            "--symbol",
            "crate::DbConnection",
            "--actor",
            "anchor-integration",
        ],
    )?;
    assert_eq!(applied["data"]["schema"], "ee.memory.anchors.v1");
    assert_eq!(applied["data"]["status"], "attached");
    assert_eq!(applied["data"]["addedAnchorCount"], 2);
    assert_eq!(applied["data"]["persisted"], true);
    assert!(applied["data"]["auditId"].is_string());
    assert!(applied["data"]["indexJobId"].is_string());
    let descriptors = applied["data"]["anchors"]
        .as_array()
        .ok_or("anchor descriptors")?;
    let locators = descriptors
        .iter()
        .filter_map(|anchor| anchor["locator"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        locators,
        std::collections::BTreeSet::from(["src/db/mod.rs", "crate::DbConnection"])
    );
    for descriptor in descriptors {
        let redacted = descriptor["redactedAnchorValue"]
            .as_str()
            .ok_or("redacted anchor identity")?;
        assert!(!redacted.contains("src/db/mod.rs") && !redacted.contains("crate::DbConnection"));
    }
    let data = serde_json::to_string(&applied["data"]).map_err(|error| error.to_string())?;
    assert!(!data.contains(&before.memory.content));
    assert!(!data.contains("test://recall-golden"));
    assert_eq!(anchor_source_snapshot(workspace.path(), MEM_RULE)?, before);
    assert_path_recalls(workspace.path(), "src/db/mod.rs", &[MEM_RULE])?;
    let by_symbol = anchor_cli(
        workspace.path(),
        &["recall", "--symbol", "crate::DbConnection"],
    )?;
    assert_eq!(item_memory_ids(&by_symbol), vec![MEM_RULE.to_owned()]);

    let db =
        ee::db::DbConnection::open_file_read_only(&db_path).map_err(|error| error.to_string())?;
    let anchors = db
        .list_memory_anchors(MEM_RULE)
        .map_err(|error| error.to_string())?;
    assert!(
        anchors
            .iter()
            .any(|anchor| anchor.anchor_kind == MemoryAnchorKind::ConfigKey)
    );
    assert_eq!(
        db.get_session(&session_id)
            .map_err(|error| error.to_string())?,
        session_before
    );
    let audits = db
        .list_audit_by_target("memory", MEM_RULE, None)
        .map_err(|error| error.to_string())?;
    assert!(
        audits
            .iter()
            .any(|audit| audit.actor.as_deref() == Some("anchor-integration"))
    );
    let jobs = db
        .list_search_index_jobs(FIXTURE_WORKSPACE_ID, None)
        .map_err(|error| error.to_string())?;
    assert!(
        jobs.iter()
            .any(|job| Some(job.id.as_str()) == applied["data"]["indexJobId"].as_str())
    );
    db.close().map_err(|error| error.to_string())?;

    let repeated = anchor_cli(
        workspace.path(),
        &[
            "memory",
            "anchors",
            MEM_RULE,
            "--path",
            "src/db/mod.rs",
            "--symbol",
            "crate::DbConnection",
        ],
    )?;
    assert_eq!(repeated["data"]["status"], "unchanged");
    assert_eq!(repeated["data"]["addedAnchorCount"], 0);
    assert_eq!(repeated["data"]["persisted"], false);
    let db =
        ee::db::DbConnection::open_file_read_only(&db_path).map_err(|error| error.to_string())?;
    assert_eq!(
        db.list_memory_anchors(MEM_RULE)
            .map_err(|error| error.to_string())?,
        anchors
    );
    assert_eq!(
        db.list_audit_by_target("memory", MEM_RULE, None)
            .map_err(|error| error.to_string())?,
        audits
    );
    assert_eq!(
        db.list_search_index_jobs(FIXTURE_WORKSPACE_ID, None)
            .map_err(|error| error.to_string())?,
        jobs
    );
    db.close().map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn code_anchor_bindings_survive_rebuild_and_recover_an_empty_projection() -> TestResult {
    let workspace = seed_workspace(
        FIXTURE_WORKSPACE_ID,
        &[(
            MEM_RULE,
            "semantic",
            "fact",
            "Keep source identity intact.",
            0.9,
        )],
    )?;
    let before = anchor_source_snapshot(workspace.path(), MEM_RULE)?;
    anchor_cli(
        workspace.path(),
        &[
            "memory",
            "anchors",
            MEM_RULE,
            "--path",
            "src/rebuild.rs",
            "--symbol",
            "Rebuild::publish",
        ],
    )?;
    anchor_cli(workspace.path(), &["index", "rebuild"])?;
    assert_path_recalls(workspace.path(), "src/rebuild.rs", &[MEM_RULE])?;
    assert_eq!(anchor_source_snapshot(workspace.path(), MEM_RULE)?, before);

    // Lose both derived tables in this disposable fixture. Durable bindings
    // still exist, so rebuilding must recover them without altering the body.
    let db = ee::db::DbConnection::open_file(workspace.path().join(".ee/ee.db"))
        .map_err(|error| error.to_string())?;
    db.execute_raw("DELETE FROM memory_anchor_index")
        .map_err(|error| error.to_string())?;
    db.execute_raw("DELETE FROM memory_anchors")
        .map_err(|error| error.to_string())?;
    assert!(
        db.list_memory_anchors(MEM_RULE)
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    assert_eq!(
        db.list_memory_code_anchor_sources(MEM_RULE)
            .map_err(|error| error.to_string())?
            .len(),
        2
    );
    db.close().map_err(|error| error.to_string())?;
    let listing = anchor_cli(workspace.path(), &["memory", "anchors", MEM_RULE])?;
    let locators = listing["data"]["anchors"]
        .as_array()
        .ok_or("source-only anchor listing")?
        .iter()
        .filter_map(|anchor| anchor["locator"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        locators,
        std::collections::BTreeSet::from(["src/rebuild.rs", "Rebuild::publish"])
    );
    let listing_json = listing["data"].to_string();
    assert!(!listing_json.contains(&before.memory.content));
    assert!(!listing_json.contains("test://recall-golden"));
    let stale = anchor_cli(workspace.path(), &["recall", "--path", "src/rebuild.rs"])?;
    let codes = degraded_codes(&stale);
    assert!(
        codes.iter().any(|code| code == "anchor_index_stale"),
        "{stale}"
    );
    assert!(
        !codes.iter().any(|code| code == "anchor_index_empty"),
        "durable anchors are present: {stale}"
    );
    anchor_cli(workspace.path(), &["index", "rebuild"])?;
    assert_path_recalls(workspace.path(), "src/rebuild.rs", &[MEM_RULE])?;
    let symbol = anchor_cli(
        workspace.path(),
        &["recall", "--symbol", "Rebuild::publish"],
    )?;
    assert_eq!(item_memory_ids(&symbol), vec![MEM_RULE.to_owned()]);
    assert!(
        !degraded_codes(&symbol)
            .iter()
            .any(|code| code == "anchor_index_stale")
    );
    assert_eq!(anchor_source_snapshot(workspace.path(), MEM_RULE)?, before);
    Ok(())
}

#[test]
fn code_anchor_backfill_is_previewable_bounded_and_idempotent() -> TestResult {
    const LEGACY: &str = "mem_00000000000000000000000044";
    let workspace = seed_workspace(
        FIXTURE_WORKSPACE_ID,
        &[
            (
                MEM_RULE,
                "semantic",
                "fact",
                "Review src/db/mod.rs before changing transaction behavior.",
                0.9,
            ),
            (
                MEM_FAILURE,
                "episodic",
                "failure",
                "The regression started in src/search/mod.rs.",
                0.8,
            ),
            (
                MEM_DECISION,
                "semantic",
                "decision",
                "Connection options use `db.cache_size`.",
                0.7,
            ),
            (
                LEGACY,
                "semantic",
                "fact",
                "Legacy reference anchor:path:src/legacy.rs anchor:symbol:Legacy::restore",
                0.8,
            ),
        ],
    )?;
    let db = ee::db::DbConnection::open_file(workspace.path().join(".ee/ee.db"))
        .map_err(|error| error.to_string())?;
    // A memory imported before extraction existed retains its original body.
    db.execute_raw("DELETE FROM memory_anchors WHERE memory_id = 'mem_00000000000000000000000044'")
        .map_err(|error| error.to_string())?;
    db.execute_raw(
        "DELETE FROM memory_anchor_index WHERE memory_id = 'mem_00000000000000000000000044'",
    )
    .map_err(|error| error.to_string())?;
    db.close().map_err(|error| error.to_string())?;
    let sources = [MEM_RULE, MEM_FAILURE, MEM_DECISION, LEGACY]
        .into_iter()
        .map(|id| anchor_source_snapshot(workspace.path(), id))
        .collect::<Result<Vec<_>, _>>()?;
    assert_path_recalls(workspace.path(), "src/**", &[])?;

    let files_before = durable_recall_files(workspace.path())?;
    let preview = anchor_cli(
        workspace.path(),
        &["index", "backfill-anchors", "--limit", "1"],
    )?;
    assert_eq!(preview["data"]["schema"], "ee.index.backfill_anchors.v1");
    assert_eq!(preview["data"]["dryRun"], true);
    assert_eq!(preview["data"]["scannedMemoryCount"], 1);
    assert_eq!(preview["data"]["proposedMemoryCount"], 1);
    assert_eq!(preview["data"]["appliedMemoryCount"], 0);
    assert_eq!(preview["data"]["nextAfter"], MEM_RULE);
    assert_eq!(durable_recall_files(workspace.path())?, files_before);
    assert_path_recalls(workspace.path(), "src/db/mod.rs", &[])?;

    let first = anchor_cli(
        workspace.path(),
        &["index", "backfill-anchors", "--limit", "1", "--apply"],
    )?;
    assert_eq!(first["data"]["appliedMemoryCount"], 1);
    assert_eq!(first["data"]["appliedAnchorCount"], 1);
    assert_eq!(first["data"]["nextAfter"], MEM_RULE);
    assert_path_recalls(workspace.path(), "src/db/mod.rs", &[MEM_RULE])?;
    assert_path_recalls(workspace.path(), "src/search/mod.rs", &[])?;

    let remainder = anchor_cli(
        workspace.path(),
        &[
            "index",
            "backfill-anchors",
            "--after",
            MEM_RULE,
            "--limit",
            "10",
            "--apply",
        ],
    )?;
    assert_eq!(remainder["data"]["scannedMemoryCount"], 3);
    assert_eq!(remainder["data"]["appliedMemoryCount"], 2);
    assert_eq!(remainder["data"]["appliedAnchorCount"], 3);
    assert!(remainder["data"]["nextAfter"].is_null());
    assert_path_recalls(workspace.path(), "src/search/mod.rs", &[MEM_FAILURE])?;
    assert_path_recalls(workspace.path(), "src/legacy.rs", &[LEGACY])?;
    let legacy_symbol = anchor_cli(workspace.path(), &["recall", "--symbol", "Legacy::restore"])?;
    assert_eq!(item_memory_ids(&legacy_symbol), vec![LEGACY.to_owned()]);

    let repeated = anchor_cli(workspace.path(), &["index", "backfill-anchors", "--apply"])?;
    assert_eq!(repeated["data"]["proposedMemoryCount"], 0);
    assert_eq!(repeated["data"]["appliedMemoryCount"], 0);
    assert_eq!(repeated["data"]["appliedAnchorCount"], 0);
    for (id, before) in [MEM_RULE, MEM_FAILURE, MEM_DECISION, LEGACY]
        .into_iter()
        .zip(sources)
    {
        assert_eq!(anchor_source_snapshot(workspace.path(), id)?, before);
    }
    let db = ee::db::DbConnection::open_file_read_only(workspace.path().join(".ee/ee.db"))
        .map_err(|error| error.to_string())?;
    assert!(
        db.list_memory_anchors(MEM_DECISION)
            .map_err(|error| error.to_string())?
            .iter()
            .all(|anchor| !matches!(
                anchor.anchor_kind,
                MemoryAnchorKind::Path | MemoryAnchorKind::Symbol
            ))
    );
    db.close().map_err(|error| error.to_string())?;
    anchor_cli(workspace.path(), &["index", "rebuild"])?;
    assert_path_recalls(workspace.path(), "src/db/mod.rs", &[MEM_RULE])?;
    assert_path_recalls(workspace.path(), "src/legacy.rs", &[LEGACY])?;
    Ok(())
}

#[test]
fn code_anchor_attachment_rejects_unsafe_values_before_any_batch_write() -> TestResult {
    let workspace = seed_workspace(
        FIXTURE_WORKSPACE_ID,
        &[(
            MEM_RULE,
            "semantic",
            "fact",
            "Keep this original observation.",
            0.9,
        )],
    )?;
    let before = anchor_source_snapshot(workspace.path(), MEM_RULE)?;
    let workspace_arg = workspace.path().to_str().ok_or("workspace path")?;
    let secret = format!("ghp_{}", "a".repeat(36));
    let secret_path = format!("src/{secret}.rs");
    for (flag, invalid) in [
        ("--path", "../outside.rs"),
        ("--path", "/tmp/outside.rs"),
        ("--path", "src/../../outside.rs"),
        ("--path", "src/*.rs"),
        ("--path", "https://example.invalid/repo.rs"),
        ("--symbol", "name with spaces"),
        ("--symbol", "crate::::Broken"),
        ("--symbol", "123Invalid"),
        ("--path", secret_path.as_str()),
        ("--symbol", secret.as_str()),
    ] {
        let output = run_ee(&[
            "--workspace",
            workspace_arg,
            "--json",
            "memory",
            "anchors",
            MEM_RULE,
            "--path",
            "src/valid.rs",
            flag,
            invalid,
        ])?;
        assert!(
            !output.status.success(),
            "unsafe anchor value must fail validation"
        );
        let response: Value =
            serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
        assert_eq!(response["schema"], "ee.error.v2");
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(&secret),
            "errors must not echo credential-like locators"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&secret));
        assert_eq!(anchor_source_snapshot(workspace.path(), MEM_RULE)?, before);
        let db = ee::db::DbConnection::open_file_read_only(workspace.path().join(".ee/ee.db"))
            .map_err(|error| error.to_string())?;
        assert!(
            db.list_memory_anchors(MEM_RULE)
                .map_err(|error| error.to_string())?
                .is_empty(),
            "the preceding valid path must not be partially attached"
        );
        assert!(
            db.list_audit_by_target("memory", MEM_RULE, None)
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        assert!(
            db.list_search_index_jobs(FIXTURE_WORKSPACE_ID, None)
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        db.close().map_err(|error| error.to_string())?;
    }
    assert_path_recalls(workspace.path(), "src/valid.rs", &[])?;
    Ok(())
}

#[test]
fn code_anchor_backfill_and_attachment_preserve_workspace_and_lifecycle_boundaries() -> TestResult {
    const SEALED: &str = "mem_00000000000000000000000044";
    const FOREIGN: &str = "mem_00000000000000000000000045";
    let workspace = seed_workspace(
        FIXTURE_WORKSPACE_ID,
        &[
            (
                MEM_RULE,
                "semantic",
                "fact",
                "Inspect src/live.rs before changing the live behavior.",
                0.9,
            ),
            (
                MEM_FAILURE,
                "semantic",
                "fact",
                "Old observation about src/tombstoned.rs.",
                0.8,
            ),
            (
                MEM_DECISION,
                "semantic",
                "fact",
                "Superseded observation about src/history.rs.",
                0.8,
            ),
            (
                SEALED,
                "semantic",
                "fact",
                "Sealed legacy observation about src/sealed.rs.",
                0.8,
            ),
        ],
    )?;
    let foreign_workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let db_path = workspace.path().join(".ee/ee.db");
    let db = ee::db::DbConnection::open_file(&db_path).map_err(|error| error.to_string())?;
    assert!(
        db.tombstone_memory(MEM_FAILURE)
            .map_err(|error| error.to_string())?
    );
    db.execute_raw("UPDATE memories SET superseded_at = '2020-01-01T00:00:00Z' WHERE id = 'mem_00000000000000000000000043'").map_err(|error| error.to_string())?;
    db.insert_memory_seal(
        SEALED,
        &ee::models::memory_seal_commitment(b"Sealed legacy observation about src/sealed.rs."),
        "2020-01-01T00:00:00Z",
    )
    .map_err(|error| error.to_string())?;
    db.insert_workspace(
        EMPTY_WORKSPACE_ID,
        &ee::db::CreateWorkspaceInput {
            path: foreign_workspace.path().to_string_lossy().into_owned(),
            name: Some("anchor-foreign-fixture".to_owned()),
        },
    )
    .map_err(|error| error.to_string())?;
    db.insert_memory(
        FOREIGN,
        &ee::db::CreateMemoryInput {
            workspace_id: EMPTY_WORKSPACE_ID.to_owned(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            content: "An unrelated workspace mentions src/foreign.rs.".to_owned(),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.7,
            importance: 0.7,
            provenance_uri: Some("manual://foreign-anchor-fixture".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: None,
            valid_to: None,
        },
    )
    .map_err(|error| error.to_string())?;
    let foreign_before = db.get_memory(FOREIGN).map_err(|error| error.to_string())?;
    db.close().map_err(|error| error.to_string())?;

    // Sharing a database never grants a workspace authority to change another
    // workspace's memory, even if the caller knows its full identifier.
    let denied = run_ee(&[
        "--workspace",
        foreign_workspace
            .path()
            .to_str()
            .ok_or("foreign workspace path")?,
        "--json",
        "memory",
        "anchors",
        MEM_RULE,
        "--database",
        db_path.to_str().ok_or("database path")?,
        "--path",
        "src/cross-workspace.rs",
    ])?;
    assert!(
        !denied.status.success(),
        "explicit database overrides must preserve workspace ownership"
    );
    for memory_id in [MEM_FAILURE, MEM_DECISION, SEALED] {
        let denied = run_ee(&[
            "--workspace",
            workspace.path().to_str().ok_or("workspace path")?,
            "--json",
            "memory",
            "anchors",
            memory_id,
            "--path",
            "src/forbidden.rs",
        ])?;
        assert!(
            !denied.status.success(),
            "inactive or sealed source cannot gain anchors: {memory_id}"
        );
    }
    let preview = anchor_cli(workspace.path(), &["index", "backfill-anchors"])?;
    assert_eq!(preview["data"]["proposedMemoryCount"], 1);
    let applied = anchor_cli(workspace.path(), &["index", "backfill-anchors", "--apply"])?;
    assert_eq!(applied["data"]["appliedMemoryCount"], 1);
    assert_path_recalls(workspace.path(), "src/live.rs", &[MEM_RULE])?;
    let db =
        ee::db::DbConnection::open_file_read_only(&db_path).map_err(|error| error.to_string())?;
    for memory_id in [MEM_FAILURE, MEM_DECISION, SEALED, FOREIGN] {
        assert!(
            db.list_memory_anchors(memory_id)
                .map_err(|error| error.to_string())?
                .is_empty(),
            "backfill must not annotate ineligible memory {memory_id}"
        );
    }
    assert_eq!(
        db.get_memory(FOREIGN).map_err(|error| error.to_string())?,
        foreign_before
    );
    db.close().map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn code_anchor_binding_follows_the_current_memory_revision() -> TestResult {
    let workspace = seed_workspace(
        FIXTURE_WORKSPACE_ID,
        &[(
            MEM_RULE,
            "semantic",
            "fact",
            "Original transaction guidance.",
            0.9,
        )],
    )?;
    anchor_cli(
        workspace.path(),
        &[
            "memory",
            "anchors",
            MEM_RULE,
            "--path",
            "src/revision.rs",
            "--symbol",
            "Revision::commit",
        ],
    )?;
    let revision = anchor_cli(
        workspace.path(),
        &[
            "memory",
            "revise",
            MEM_RULE,
            "--content",
            "Corrected transaction guidance.",
            "--reason",
            "correction",
        ],
    )?;
    let new_id = revision["data"]["new_id"]
        .as_str()
        .ok_or("revision must return new_id")?;
    assert_ne!(new_id, MEM_RULE);
    assert_path_recalls(workspace.path(), "src/revision.rs", &[new_id])?;
    let source = anchor_source_snapshot(workspace.path(), new_id)?;
    assert_eq!(source.memory.content, "Corrected transaction guidance.");
    assert_eq!(
        source.memory.provenance_uri.as_deref(),
        Some("test://recall-golden")
    );
    assert_eq!(source.logical_id.as_deref(), Some(MEM_RULE));
    let original = anchor_source_snapshot(workspace.path(), MEM_RULE)?;
    assert_eq!(original.memory.content, "Original transaction guidance.");
    assert!(original.superseded_at.is_some());
    anchor_cli(workspace.path(), &["index", "rebuild"])?;
    assert_path_recalls(workspace.path(), "src/revision.rs", &[new_id])?;
    let symbol = anchor_cli(
        workspace.path(),
        &["recall", "--symbol", "Revision::commit"],
    )?;
    assert_eq!(item_memory_ids(&symbol), vec![new_id.to_owned()]);
    Ok(())
}

#[test]
fn code_anchor_public_schemas_describe_actual_listing_and_backfill_preview() -> TestResult {
    let workspace = seed_workspace(
        FIXTURE_WORKSPACE_ID,
        &[(
            MEM_RULE,
            "semantic",
            "fact",
            "Review src/schema.rs before changing the response.",
            0.9,
        )],
    )?;
    let files_before = durable_recall_files(workspace.path())?;
    let listing = anchor_cli(workspace.path(), &["memory", "anchors", MEM_RULE])?;
    assert_eq!(listing["data"]["status"], "listed");
    assert_eq!(listing["data"]["persisted"], false);
    let backfill = anchor_cli(
        workspace.path(),
        &["index", "backfill-anchors", "--dry-run"],
    )?;
    assert_eq!(durable_recall_files(workspace.path())?, files_before);
    for (id, response) in [
        ("ee.memory.anchors.v1", listing),
        ("ee.index.backfill_anchors.v1", backfill),
    ] {
        let schema = anchor_cli(workspace.path(), &["schema", "export", id])?;
        assert_eq!(
            schema["$id"],
            format!("https://eidetic-engine/schemas/{id}.json")
        );
        assert_eq!(schema["properties"]["schema"]["const"], id);
        assert_eq!(response["data"]["schema"], id);
        for field in string_set(&schema, "/required")? {
            assert!(
                response["data"].get(&field).is_some(),
                "{id} report omitted required field {field}"
            );
        }
        assert!(response["data"]["dryRun"].is_boolean());
    }
    Ok(())
}

fn recall_item_ids(report: &ee::core::recall::RecallReport) -> Vec<String> {
    report
        .items
        .iter()
        .map(|item| item.memory_id.clone())
        .collect()
}

fn property_rows() -> impl Strategy<Value = Vec<RecallCandidateRow>> {
    proptest::collection::vec((16_usize..320, 30_u8..=100, 0_u8..4), 1..16).prop_map(|specs| {
        specs
            .into_iter()
            .enumerate()
            .map(|(idx, (content_len, confidence, flavor))| {
                let path = format!("src/recall/item_{idx}.rs");
                let content = format!(
                    "{} anchor:path:{path}",
                    "recall budget monotonicity ".repeat(content_len / 28 + 1)
                );
                let (level, kind) = match flavor {
                    0 => ("procedural", "rule"),
                    1 => ("semantic", "decision"),
                    2 => ("episodic", "failure"),
                    _ => ("working", "note"),
                };
                RecallCandidateRow {
                    memory_id: format!("mem_recall_property_{idx:024}"),
                    anchor_kind: MemoryAnchorKind::Path,
                    normalized_path: Some(path),
                    symbol: None,
                    freshness_state: MemoryAnchorFreshnessState::Current,
                    row_generation: 7,
                    level: level.to_owned(),
                    kind: kind.to_owned(),
                    confidence: f32::from(confidence) / 100.0,
                    content,
                    tombstoned: false,
                    tags: vec!["recall-property".to_owned()],
                    provenance: vec![RecallProvenanceRef {
                        uri: "test://recall-property".to_owned(),
                        source_type: "memory_provenance".to_owned(),
                    }],
                }
            })
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn recall_ranking_is_scan_order_deterministic_and_budget_monotone(
        rows in property_rows(),
        small_budget in 0_u32..320,
        extra_budget in 0_u32..320,
    ) {
        let query = RecallQuery {
            paths: vec!["src/recall/*.rs".to_owned()],
            ..RecallQuery::default()
        };
        let forward = evaluate_recall(&query, &rows, Some(7), 7);
        let mut reversed_rows = rows.clone();
        reversed_rows.reverse();
        let reversed = evaluate_recall(&query, &reversed_rows, Some(7), 7);
        prop_assert_eq!(
            recall_item_ids(&forward),
            recall_item_ids(&reversed),
            "recall ranking must not depend on DB scan order"
        );

        let mut small_query = query.clone();
        small_query.max_tokens = Some(small_budget);
        let small = evaluate_recall(&small_query, &rows, Some(7), 7);

        let mut large_query = query;
        large_query.max_tokens = Some(small_budget.saturating_add(extra_budget));
        let large = evaluate_recall(&large_query, &rows, Some(7), 7);

        let small_ids = recall_item_ids(&small);
        let large_ids = recall_item_ids(&large);
        prop_assert!(
            large_ids.starts_with(&small_ids),
            "smaller recall budget must emit a ranked prefix; small={small_ids:?}, large={large_ids:?}"
        );
        prop_assert!(
            small.items.len() <= large.items.len(),
            "larger recall budget must not drop items that fit in the smaller budget"
        );
    }
}

#[test]
fn recall_requires_at_least_one_selector() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let output = run_ee(&[
        "recall",
        "--workspace",
        workspace.path().to_str().unwrap(),
        "--json",
    ])?;
    if output.status.success() {
        return Err("ee recall without selectors must fail with a usage error".to_owned());
    }
    if output.status.code() != Some(1) {
        return Err(format!(
            "expected usage exit code 1, got {:?}",
            output.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("requires at least one selector") {
        return Err(format!(
            "usage error must name the selector requirement; got: {stdout}"
        ));
    }
    Ok(())
}

#[test]
fn recall_rejects_zero_budget_tokens() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let output = run_ee(&[
        "recall",
        "--path",
        "src/db/*.rs",
        "--budget-tokens",
        "0",
        "--workspace",
        workspace.path().to_str().unwrap(),
        "--json",
    ])?;
    if output.status.success() {
        return Err("ee recall --budget-tokens 0 must fail with a usage error".to_owned());
    }
    if output.status.code() != Some(1) {
        return Err(format!(
            "expected usage exit code 1, got {:?}",
            output.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("--budget-tokens must be greater than zero") {
        return Err(format!(
            "usage error must name the zero-budget problem; got: {stdout}"
        ));
    }
    Ok(())
}

#[test]
fn recall_too_small_positive_budget_reports_unsatisfiable() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let response = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--budget-tokens",
            "1",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--json",
        ])?,
        "recall --budget-tokens 1",
    )?;
    if !item_memory_ids(&response).is_empty() {
        return Err("too-small positive budget must emit an empty item page".to_owned());
    }
    if response.pointer("/data/recall/truncated") != Some(&Value::Bool(true)) {
        return Err("too-small positive budget must report truncated=true".to_owned());
    }
    if response
        .pointer("/data/recall/continuationCursor")
        .is_some_and(|cursor| !cursor.is_null())
    {
        return Err("too-small positive budget must not mint a non-progress cursor".to_owned());
    }
    let codes = degraded_codes(&response);
    if !codes
        .iter()
        .any(|code| code == "output_budget_unsatisfiable")
    {
        return Err(format!(
            "expected output_budget_unsatisfiable, got {codes:?}"
        ));
    }
    Ok(())
}

#[test]
fn recall_markdown_output_matches_golden() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let output = run_ee(&[
        "recall",
        "--path",
        "src/db/*.rs",
        "--workspace",
        workspace.path().to_str().unwrap(),
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!(
            "ee recall failed: {}\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    assert_recall_golden("markdown.golden", &stdout)
}

#[test]
fn recall_json_output_matches_golden_and_schema() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let output = run_ee(&[
        "recall",
        "--path",
        "src/db/*.rs",
        "--workspace",
        workspace.path().to_str().unwrap(),
        "--json",
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!(
            "ee recall --json failed: {}\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    assert_recall_golden("json.json.golden", &(stdout.trim().to_string() + "\n"))?;

    let response: Value =
        serde_json::from_str(stdout.trim()).map_err(|error| format!("parse response: {error}"))?;
    if response.pointer("/schema").and_then(Value::as_str) != Some("ee.response.v2") {
        return Err("envelope schema must be ee.response.v2".to_owned());
    }
    let payload = response
        .pointer("/data/recall")
        .ok_or("response missing data.recall payload")?;
    let schema = load_schema()?;
    validate_against_schema(&schema, payload)?;

    // Ranking: the procedural rule (0.9) outranks the episodic failure
    // (1.0 × 0.8 × 0.6 × 1.15 = 0.552); the decision anchored on
    // src/graph/mod.rs must not match the src/db glob.
    let ids = item_memory_ids(&response);
    if ids != vec![MEM_RULE.to_owned(), MEM_FAILURE.to_owned()] {
        return Err(format!("unexpected ranked ids: {ids:?}"));
    }
    Ok(())
}

#[test]
fn recall_kind_filter_narrows_before_ranking() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let narrowed = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--kind",
            "failure",
            "--workspace",
            &workspace_arg,
            "--json",
        ])?,
        "recall --kind failure",
    )?;
    let ids = item_memory_ids(&narrowed);
    if ids != vec![MEM_FAILURE.to_owned()] {
        return Err(format!(
            "--kind failure must keep only the failure: {ids:?}"
        ));
    }
    Ok(())
}

#[test]
fn recall_symbol_level_and_or_dedup_compose() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let symbol_only = parse_response(
        &run_ee(&[
            "recall",
            "--symbol",
            "DbConnection",
            "--level",
            "episodic",
            "--workspace",
            &workspace_arg,
            "--json",
        ])?,
        "recall --symbol DbConnection --level episodic",
    )?;
    let ids = item_memory_ids(&symbol_only);
    if ids != vec![MEM_FAILURE.to_owned()] {
        return Err(format!(
            "--symbol plus --level must keep only the anchored episodic failure: {ids:?}"
        ));
    }
    if symbol_only
        .pointer("/data/recall/items/0/anchor/kind")
        .and_then(Value::as_str)
        != Some("symbol")
    {
        return Err("symbol-only recall must report a symbol anchor".to_owned());
    }

    let path_and_symbol = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--symbol",
            "DbConnection",
            "--level",
            "episodic",
            "--workspace",
            &workspace_arg,
            "--json",
        ])?,
        "recall --path src/db/*.rs --symbol DbConnection --level episodic",
    )?;
    let ids = item_memory_ids(&path_and_symbol);
    if ids != vec![MEM_FAILURE.to_owned()] {
        return Err(format!(
            "path+symbol OR composition must dedup the failure memory once: {ids:?}"
        ));
    }
    if path_and_symbol
        .pointer("/data/recall/totalMatched")
        .and_then(Value::as_u64)
        != Some(1)
    {
        return Err("path+symbol dedup must report one matched memory".to_owned());
    }
    Ok(())
}

#[test]
fn recall_distinct_empty_codes_for_filtered_vs_empty_index() -> TestResult {
    // Filters removed everything: the index HAS rows for the surface, so the
    // distinct recall_filtered_empty (not anchor_index_empty) must fire.
    let workspace = seed_recall_workspace()?;
    let filtered = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--kind",
            "convention",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--json",
        ])?,
        "recall --kind convention",
    )?;
    if !item_memory_ids(&filtered).is_empty() {
        return Err("filtered recall must return no items".to_owned());
    }
    let codes = degraded_codes(&filtered);
    if !codes.iter().any(|code| code == "recall_filtered_empty") {
        return Err(format!("expected recall_filtered_empty, got {codes:?}"));
    }
    if codes.iter().any(|code| code == "anchor_index_empty") {
        return Err("anchor_index_empty must not fire when the index has rows".to_owned());
    }

    // A workspace with no anchored memories at all reports the empty index.
    let empty = seed_workspace(EMPTY_WORKSPACE_ID, &[])?;
    let empty_response = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/**",
            "--workspace",
            empty.path().to_str().unwrap(),
            "--json",
        ])?,
        "recall over empty index",
    )?;
    let codes = degraded_codes(&empty_response);
    if !codes.iter().any(|code| code == "anchor_index_empty") {
        return Err(format!("expected anchor_index_empty, got {codes:?}"));
    }
    if codes.iter().any(|code| code == "recall_filtered_empty") {
        return Err("recall_filtered_empty must not fire for an empty index".to_owned());
    }
    Ok(())
}

#[test]
fn recall_budget_pages_partition_the_ranked_set_exactly_once() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let first = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--budget-tokens",
            "20",
            "--workspace",
            &workspace_arg,
            "--json",
        ])?,
        "recall --budget-tokens 20",
    )?;
    let first_ids = item_memory_ids(&first);
    if first_ids.is_empty() || first_ids.len() >= 2 {
        return Err(format!(
            "budget 20 must keep a strict non-empty prefix of the 2 matches: {first_ids:?}"
        ));
    }
    if first.pointer("/data/recall/truncated") != Some(&Value::Bool(true)) {
        return Err("budget page must report truncated=true".to_owned());
    }
    let truncation_entry = first
        .pointer("/degraded")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry.pointer("/code").and_then(Value::as_str) == Some("output_truncated_budget")
            })
        })
        .ok_or("budget truncation must emit output_truncated_budget")?
        .clone();
    let cursor = truncation_entry
        .pointer("/details/continuationCursor")
        .and_then(Value::as_str)
        .ok_or("output_truncated_budget must carry details.continuationCursor")?
        .to_owned();
    let dropped = truncation_entry
        .pointer("/details/droppedCount")
        .and_then(Value::as_u64)
        .ok_or("output_truncated_budget must carry details.droppedCount")?;
    if dropped as usize + first_ids.len() != 2 {
        return Err(format!(
            "droppedCount {dropped} plus kept {} must equal total 2",
            first_ids.len()
        ));
    }

    let second = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--budget-tokens",
            "20",
            "--cursor",
            &cursor,
            "--workspace",
            &workspace_arg,
            "--json",
        ])?,
        "recall --cursor resume",
    )?;
    let second_ids = item_memory_ids(&second);
    let mut union: Vec<String> = first_ids
        .iter()
        .cloned()
        .chain(second_ids.clone())
        .collect();
    let union_len = union.len();
    union.sort();
    union.dedup();
    if union.len() != union_len {
        return Err(format!(
            "pages must not duplicate items: {first_ids:?} + {second_ids:?}"
        ));
    }
    let mut expected = vec![MEM_RULE.to_owned(), MEM_FAILURE.to_owned()];
    expected.sort();
    if union != expected {
        return Err(format!(
            "pages must cover the full ranked set with no gaps: {first_ids:?} + {second_ids:?}"
        ));
    }
    Ok(())
}

#[test]
fn recall_rejects_garbage_cursor_with_empty_page() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let response = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--cursor",
            "not-a-valid-cursor",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--json",
        ])?,
        "recall garbage cursor",
    )?;
    let codes = degraded_codes(&response);
    if !codes.iter().any(|code| code == "cursor_invalid") {
        return Err(format!("expected cursor_invalid, got {codes:?}"));
    }
    if !item_memory_ids(&response).is_empty() {
        return Err("a rejected cursor must yield an empty page, never a restart".to_owned());
    }
    Ok(())
}

#[test]
fn recall_diff_outside_git_degrades_and_never_blocks() -> TestResult {
    let workspace = seed_recall_workspace()?;
    // tempdir workspaces are not git worktrees, so the read-only shell-out
    // fails and the diff selector degrades to an empty path set while the
    // command still succeeds (a recall failure must never block an edit).
    let response = parse_response(
        &run_ee(&[
            "recall",
            "--diff-staged",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--json",
        ])?,
        "recall --diff-staged outside git",
    )?;
    let codes = degraded_codes(&response);
    if !codes.iter().any(|code| code == "recall_git_unavailable") {
        return Err(format!("expected recall_git_unavailable, got {codes:?}"));
    }
    if !item_memory_ids(&response).is_empty() {
        return Err("a degraded diff selector must contribute no matches".to_owned());
    }
    Ok(())
}

fn load_schema() -> Result<Value, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("schemas")
        .join("ee.recall.v1.json");
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse schema: {error}"))
}

fn string_set(value: &Value, pointer: &str) -> Result<Vec<String>, String> {
    Ok(value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("schema missing {pointer}"))?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}

/// Minimal structural validator covering the constructs the recall schema
/// uses (required sets, const, enums); the repo has no jsonschema engine.
fn validate_against_schema(schema: &Value, payload: &Value) -> TestResult {
    for field in string_set(schema, "/required")? {
        if payload.get(&field).is_none() {
            return Err(format!("payload missing required field {field}"));
        }
    }
    if payload.pointer("/schema").and_then(Value::as_str) != Some("ee.recall.v1") {
        return Err("payload schema must be ee.recall.v1".to_owned());
    }
    for field in string_set(schema, "/properties/query/required")? {
        if payload.pointer(&format!("/query/{field}")).is_none() {
            return Err(format!("query missing required field {field}"));
        }
    }
    let freshness_enum = string_set(
        schema,
        "/properties/items/items/properties/freshnessState/enum",
    )?;
    let item_required = string_set(schema, "/properties/items/items/required")?;
    for item in payload
        .pointer("/items")
        .and_then(Value::as_array)
        .ok_or("items must be an array")?
    {
        for field in &item_required {
            if item.get(field).is_none() {
                return Err(format!("item missing required field {field}"));
            }
        }
        let freshness = item
            .pointer("/freshnessState")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !freshness_enum.iter().any(|allowed| allowed == freshness) {
            return Err(format!("freshnessState {freshness:?} not in schema enum"));
        }
        for reference in item
            .pointer("/provenance")
            .and_then(Value::as_array)
            .ok_or("item provenance must be an array")?
        {
            if reference.pointer("/uri").and_then(Value::as_str).is_none()
                || reference
                    .pointer("/sourceType")
                    .and_then(Value::as_str)
                    .is_none()
            {
                return Err("provenance entries need uri and sourceType".to_owned());
            }
        }
    }
    Ok(())
}
