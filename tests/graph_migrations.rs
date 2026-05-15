use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

use ee::db::{DatabaseConfig, DbConnection, MIGRATIONS, MigrationRecord};
use ee::graph::{CentralityRefreshOptions, CentralityRefreshStatus, refresh_graph_snapshot};
use sqlmodel_core::{Row, Value};
use sqlmodel_frankensqlite::FrankenConnection;

type TestResult = Result<(), String>;

const LEGACY_GRAPH_FIXTURE_VERSION: u32 = 40;
const GRAPH_WITNESS_VERSION: u32 = 46;
const GRAPH_RESULT_VERSION: u32 = 47;
const LEGACY_WORKSPACE_ID: &str = "wsp_0123456789abcdef0123456789";

fn seed_database_through(path: &Path, version: u32) -> TestResult {
    let connection =
        DbConnection::open_file(path).map_err(|error| format!("open seed db: {error}"))?;
    connection
        .ensure_migration_table()
        .map_err(|error| format!("ensure migration table: {error}"))?;

    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version() <= version)
    {
        connection.execute_raw(migration.sql()).map_err(|error| {
            format!("apply seed migration V{:03}: {error}", migration.version())
        })?;
        let record = MigrationRecord::new(
            migration.version(),
            migration.name(),
            migration.checksum_label(),
            "2026-05-15T00:00:00Z",
        )
        .map_err(|error| {
            format!(
                "build migration record V{:03}: {error}",
                migration.version()
            )
        })?;
        connection.record_migration(&record).map_err(|error| {
            format!("record seed migration V{:03}: {error}", migration.version())
        })?;
    }

    connection
        .close()
        .map_err(|error| format!("close seed db: {error}"))
}

fn seed_legacy_memories_and_links(path: &Path) -> TestResult {
    let connection = open_schema_reader(path)?;
    connection
        .execute_sync(
            "INSERT INTO workspaces (id, path, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            &[
                Value::Text(LEGACY_WORKSPACE_ID.to_string()),
                Value::Text("/workspace/graph-migration-legacy".to_string()),
                Value::Text("graph migration legacy".to_string()),
                Value::Text("2026-05-15T00:00:00Z".to_string()),
                Value::Text("2026-05-15T00:00:00Z".to_string()),
            ],
        )
        .map_err(|error| format!("seed legacy workspace: {error}"))?;

    for index in 0..100 {
        let memory_id = format!("mem_{index:026}");
        connection
            .execute_sync(
                "INSERT INTO memories (id, workspace_id, level, kind, content, confidence, utility, importance, provenance_uri, created_at, updated_at, trust_class, trust_subclass, provenance_chain_hash, provenance_chain_hash_version, provenance_verification_status, valid_from, valid_to) VALUES (?1, ?2, 'working', 'note', ?3, 0.8, 0.7, 0.6, ?4, '2026-05-15T00:00:00Z', '2026-05-15T00:00:00Z', 'agent_assertion', NULL, NULL, 'ee.memory.provenance_chain.v1', 'unverified', '2026-05-15T00:00:00Z', NULL)",
                &[
                    Value::Text(memory_id),
                    Value::Text(LEGACY_WORKSPACE_ID.to_string()),
                    Value::Text(format!("Legacy graph migration memory {index}")),
                    Value::Text(format!("test://graph-migration/{index}")),
                ],
            )
            .map_err(|error| format!("seed legacy memory {index}: {error}"))?;
    }

    for index in 0..99 {
        let link_id = format!("link_{index:026}");
        let src = format!("mem_{index:026}");
        let dst = format!("mem_{:026}", index + 1);
        connection
            .execute_sync(
                "INSERT INTO memory_links (id, src_memory_id, dst_memory_id, relation, weight, confidence, directed, evidence_count, last_reinforced_at, source, created_at, created_by, metadata_json) VALUES (?1, ?2, ?3, 'supports', 1.0, 0.9, 1, 1, NULL, 'agent', '2026-05-15T00:00:00Z', 'graph-migration-test', NULL)",
                &[Value::Text(link_id), Value::Text(src), Value::Text(dst)],
            )
            .map_err(|error| format!("seed legacy memory link {index}: {error}"))?;
    }

    connection
        .close_sync()
        .map_err(|error| format!("close legacy seed reader: {error}"))
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn row_text(row: &Row, index: usize, context: &str) -> Result<String, String> {
    row.get(index)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{context}: expected text at column {index}"))
}

fn open_schema_reader(path: &Path) -> Result<FrankenConnection, String> {
    FrankenConnection::open_file(path.to_string_lossy().into_owned())
        .map_err(|error| format!("open schema reader: {error}"))
}

fn query_text_set(connection: &FrankenConnection, sql: &str) -> Result<BTreeSet<String>, String> {
    let rows = connection
        .query_sync(sql, &[] as &[Value])
        .map_err(|error| format!("query text set: {error}; sql={sql}"))?;
    let mut values = BTreeSet::new();
    for row in &rows {
        values.insert(row_text(row, 0, sql)?);
    }
    Ok(values)
}

fn query_count(connection: &FrankenConnection, sql: &str, params: &[Value]) -> Result<u64, String> {
    let rows = connection
        .query_sync(sql, params)
        .map_err(|error| format!("query count: {error}; sql={sql}"))?;
    rows.first()
        .and_then(|row| row.get(0))
        .and_then(Value::as_i64)
        .and_then(|count| u64::try_from(count).ok())
        .ok_or_else(|| format!("count query did not return a non-negative integer: {sql}"))
}

fn table_columns(connection: &FrankenConnection, table: &str) -> Result<BTreeSet<String>, String> {
    let sql = format!("PRAGMA table_info({})", quote_identifier(table));
    let rows = connection
        .query_sync(&sql, &[] as &[Value])
        .map_err(|error| format!("query columns for {table}: {error}"))?;
    rows.iter()
        .map(|row| row_text(row, 1, table))
        .collect::<Result<BTreeSet<_>, _>>()
}

fn index_names(connection: &FrankenConnection, table: &str) -> Result<BTreeSet<String>, String> {
    query_text_set(
        connection,
        &format!(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = '{table}' ORDER BY name"
        ),
    )
}

fn ensure_contains(set: &BTreeSet<String>, value: &str, context: &str) -> TestResult {
    if set.contains(value) {
        Ok(())
    } else {
        Err(format!("{context}: missing {value}; got {set:?}"))
    }
}

fn ensure_graph_algorithm_tables(connection: &FrankenConnection) -> TestResult {
    let tables = query_text_set(
        connection,
        "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
    )?;
    ensure_contains(
        &tables,
        "graph_algorithm_witnesses",
        "graph algorithm tables",
    )?;
    ensure_contains(&tables, "graph_algorithm_results", "graph algorithm tables")?;

    let witness_columns = table_columns(connection, "graph_algorithm_witnesses")?;
    for column in [
        "workspace_id",
        "snapshot_id",
        "algorithm",
        "params_json",
        "witness_json",
        "recorded_at",
    ] {
        ensure_contains(
            &witness_columns,
            column,
            "graph_algorithm_witnesses columns",
        )?;
    }

    let result_columns = table_columns(connection, "graph_algorithm_results")?;
    for column in [
        "workspace_id",
        "snapshot_id",
        "algorithm",
        "params_hash",
        "result_json",
        "computed_at",
        "ttl_seconds",
    ] {
        ensure_contains(&result_columns, column, "graph_algorithm_results columns")?;
    }

    let witness_indexes = index_names(connection, "graph_algorithm_witnesses")?;
    ensure_contains(
        &witness_indexes,
        "idx_graph_algorithm_witnesses_lookup",
        "graph_algorithm_witnesses indexes",
    )?;

    let result_indexes = index_names(connection, "graph_algorithm_results")?;
    ensure_contains(
        &result_indexes,
        "idx_graph_algorithm_results_lookup",
        "graph_algorithm_results indexes",
    )?;
    ensure_contains(
        &result_indexes,
        "idx_graph_algorithm_results_computed",
        "graph_algorithm_results indexes",
    )?;

    Ok(())
}

#[test]
fn graph_algorithm_migrations_apply_from_pre_algorithm_schema() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let database_path = tempdir.path().join("graph-algorithm-migrations.db");
    seed_database_through(&database_path, LEGACY_GRAPH_FIXTURE_VERSION)?;
    seed_legacy_memories_and_links(&database_path)?;

    let connection =
        DbConnection::open_file(&database_path).map_err(|error| format!("open db: {error}"))?;
    let result = connection
        .migrate()
        .map_err(|error| format!("migrate pre-graph-algorithm db: {error}"))?;

    if !result.applied().contains(&GRAPH_WITNESS_VERSION) {
        return Err(format!(
            "V046 must be applied from a V040 legacy database; applied={:?}",
            result.applied()
        ));
    }
    if !result.applied().contains(&GRAPH_RESULT_VERSION) {
        return Err(format!(
            "V047 must be applied from a V040 legacy database; applied={:?}",
            result.applied()
        ));
    }
    let witness_rows = connection
        .count_table_rows("graph_algorithm_witnesses")
        .map_err(|error| format!("count witness rows: {error}"))?;
    let result_rows = connection
        .count_table_rows("graph_algorithm_results")
        .map_err(|error| format!("count result rows: {error}"))?;
    if witness_rows != 0 || result_rows != 0 {
        return Err(format!(
            "graph algorithm migrations must not backfill derived rows, got witnesses={witness_rows}, results={result_rows}"
        ));
    }
    let memory_rows = connection
        .count_table_rows("memories")
        .map_err(|error| format!("count memories: {error}"))?;
    let link_rows = connection
        .count_table_rows("memory_links")
        .map_err(|error| format!("count memory links: {error}"))?;
    if memory_rows != 100 || link_rows != 99 {
        return Err(format!(
            "legacy workspace fixture must preserve 100 memories and 99 links after migration, got memories={memory_rows}, links={link_rows}"
        ));
    }

    connection
        .close()
        .map_err(|error| format!("close db: {error}"))?;

    let schema_reader = open_schema_reader(&database_path)?;
    ensure_graph_algorithm_tables(&schema_reader)?;
    schema_reader
        .close_sync()
        .map_err(|error| format!("close schema reader: {error}"))
}

#[test]
fn graph_algorithm_migrations_are_idempotent_after_apply() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let database_path = tempdir.path().join("graph-algorithm-idempotent.db");
    seed_database_through(&database_path, LEGACY_GRAPH_FIXTURE_VERSION)?;

    let connection =
        DbConnection::open_file(&database_path).map_err(|error| format!("open db: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("first migrate: {error}"))?;
    let second = connection
        .migrate()
        .map_err(|error| format!("second migrate: {error}"))?;

    if !second.applied().is_empty() {
        return Err(format!(
            "second migrate must not apply new migrations, applied={:?}",
            second.applied()
        ));
    }
    for version in [GRAPH_WITNESS_VERSION, GRAPH_RESULT_VERSION] {
        if !second.skipped().contains(&version) {
            return Err(format!(
                "second migrate must skip V{version:03}; skipped={:?}",
                second.skipped()
            ));
        }
        let recorded = connection
            .applied_migrations()
            .map_err(|error| format!("list applied migrations: {error}"))?
            .iter()
            .filter(|record| record.version() == version)
            .count();
        if recorded != 1 {
            return Err(format!(
                "V{version:03} must have exactly one migration record, got {recorded}"
            ));
        }
    }

    connection
        .close()
        .map_err(|error| format!("close db: {error}"))
}

#[test]
fn graph_snapshot_refresh_populates_witnesses_after_legacy_migration() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let database_path = tempdir.path().join("graph-algorithm-refresh.db");
    seed_database_through(&database_path, LEGACY_GRAPH_FIXTURE_VERSION)?;
    seed_legacy_memories_and_links(&database_path)?;

    let connection =
        DbConnection::open_file(&database_path).map_err(|error| format!("open db: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate pre-refresh db: {error}"))?;

    let witness_rows_before = connection
        .count_table_rows("graph_algorithm_witnesses")
        .map_err(|error| format!("count witness rows before refresh: {error}"))?;
    if witness_rows_before != 0 {
        return Err(format!(
            "legacy migration must not backfill witness rows before graph refresh, got {witness_rows_before}"
        ));
    }

    let refresh = refresh_graph_snapshot(
        &connection,
        LEGACY_WORKSPACE_ID,
        &CentralityRefreshOptions::default(),
    )
    .map_err(|error| format!("refresh graph snapshot after migration: {error}"))?;
    if refresh.centrality.status != CentralityRefreshStatus::Refreshed {
        return Err(format!(
            "graph refresh after legacy migration must compute centrality, got {:?}",
            refresh.centrality.status
        ));
    }
    let snapshot = refresh
        .snapshot
        .ok_or_else(|| "graph refresh after legacy migration must persist a snapshot".to_owned())?;
    let witnesses = connection
        .list_graph_algorithm_witnesses(LEGACY_WORKSPACE_ID, &snapshot.id, Some("pagerank"))
        .map_err(|error| format!("list pagerank witnesses after refresh: {error}"))?;
    if witnesses.len() != 1 {
        return Err(format!(
            "graph refresh must emit exactly one pagerank witness for the new snapshot, got {}",
            witnesses.len()
        ));
    }

    connection
        .close()
        .map_err(|error| format!("close db: {error}"))
}

#[test]
fn concurrent_graph_algorithm_migration_records_each_version_once() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let database_path = tempdir.path().join("graph-algorithm-concurrent.db");
    seed_database_through(&database_path, LEGACY_GRAPH_FIXTURE_VERSION)?;

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let barrier = Arc::clone(&barrier);
        let database_path = database_path.clone();
        handles.push(thread::spawn(
            move || -> Result<(Vec<u32>, Vec<u32>), String> {
                barrier.wait();
                let connection = DbConnection::open(DatabaseConfig::file(database_path))
                    .map_err(|error| format!("open concurrent db: {error}"))?;
                let result = connection
                    .migrate()
                    .map_err(|error| format!("concurrent migrate: {error}"))?;
                connection
                    .close()
                    .map_err(|error| format!("close concurrent db: {error}"))?;
                Ok((result.applied().to_vec(), result.skipped().to_vec()))
            },
        ));
    }

    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(
            handle
                .join()
                .map_err(|_| "concurrent migration thread panicked".to_string())??,
        );
    }

    for version in [GRAPH_WITNESS_VERSION, GRAPH_RESULT_VERSION] {
        let applied_count = outcomes
            .iter()
            .filter(|(applied, _)| applied.contains(&version))
            .count();
        let skipped_count = outcomes
            .iter()
            .filter(|(_, skipped)| skipped.contains(&version))
            .count();
        if applied_count != 1 || skipped_count != 1 {
            return Err(format!(
                "V{version:03} must have one applier and one skipper across concurrent migrations; outcomes={outcomes:?}"
            ));
        }
    }

    let schema_reader = open_schema_reader(&database_path)?;
    ensure_graph_algorithm_tables(&schema_reader)?;
    for version in [GRAPH_WITNESS_VERSION, GRAPH_RESULT_VERSION] {
        let recorded = query_count(
            &schema_reader,
            "SELECT COUNT(*) FROM ee_schema_migrations WHERE version = ?1",
            &[Value::BigInt(i64::from(version))],
        )?;
        if recorded != 1 {
            return Err(format!(
                "V{version:03} must have exactly one migration record after concurrent runs, got {recorded}"
            ));
        }
    }
    schema_reader
        .close_sync()
        .map_err(|error| format!("close schema reader: {error}"))
}
