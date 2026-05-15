# Graph Algorithm Tables Migration Contract

Status: active
Bead: bd-bife.12
Owner surface: `src/db/mod.rs`

## Purpose

Graph algorithm output is derived state. Memories, links, causal evidence,
revisions, and rules remain the source of truth; graph snapshots, algorithm
witnesses, and cached algorithm results can be rebuilt from those sources.

Two migrations add the graph algorithm persistence layer:

| Version | Name | Table | Role |
| --- | --- | --- | --- |
| V046 | `graph_algorithm_witnesses` | `graph_algorithm_witnesses` | Append CGSE witness evidence for one algorithm run against one graph snapshot. |
| V047 | `graph_algorithm_results` | `graph_algorithm_results` | Cache deterministic algorithm results keyed by workspace, snapshot, algorithm, and params hash. |

## Table Contracts

`graph_algorithm_witnesses` stores one row per recorded complexity witness:

- `workspace_id` references `workspaces(id)` with cascade delete.
- `snapshot_id` references `graph_snapshots(id)` with cascade delete.
- `algorithm` is a non-empty algorithm name.
- `params_json` is canonical JSON for the algorithm parameters.
- `witness_json` is canonical JSON for elapsed time, sampling choice, decision
  path hash, and related CGSE evidence.
- `recorded_at` is a non-empty timestamp.
- `idx_graph_algorithm_witnesses_lookup` supports
  `(workspace_id, snapshot_id, algorithm)` lookups.

`graph_algorithm_results` stores reusable cache rows:

- `workspace_id` references `workspaces(id)` with cascade delete.
- `snapshot_id` references `graph_snapshots(id)` with cascade delete.
- `algorithm` is a non-empty algorithm name.
- `params_hash` is a `blake3:*` hash of canonical parameters.
- `result_json` is canonical JSON for the algorithm result.
- `computed_at` is a non-empty timestamp.
- `ttl_seconds` is positive.
- Primary key: `(workspace_id, snapshot_id, algorithm, params_hash)`.
- `idx_graph_algorithm_results_lookup` supports algorithm cache lookups.
- `idx_graph_algorithm_results_computed` supports maintenance by compute time.

## Migration Behavior

Fresh databases apply V046 and V047 in the normal contiguous migration sequence.
Existing workspaces apply the same migrations without backfilling algorithm rows:
after migration, no witness or result rows exist until a graph operation runs.

The migration runner is idempotent. A second migration pass skips already
recorded versions, and `apply_migration` rechecks the migration table inside the
write gate before applying DDL. That recheck is the concurrency contract: if two
processes race to migrate the same database, one records the migration and the
other observes it as already applied rather than duplicating schema changes.

File databases also use the process/file write-owner gate in `DbConnection` so
write transactions serialize across local threads and processes before SQLite
contention retry handling is needed.

## Verification Hooks

The current executable coverage for this contract is:

- `tests/graph_migrations.rs` is a standalone integration target that applies
  migrations from a V040 legacy database seeded with 100 memories and 99
  `memory_links`, verifies V046/V047
  table/index/column shape, asserts no derived witness/result rows are
  backfilled, verifies `ee graph snapshot refresh`-equivalent code persists a
  memory-links snapshot plus one pagerank witness, checks idempotent reruns, and
  exercises a two-thread file database migration race.
- `src/db/mod.rs::migration_sequence_is_contiguous` verifies the compiled
  migration list is ordered and contiguous.
- `src/db/mod.rs::migrate_creates_expected_tables` verifies
  `graph_algorithm_witnesses` and `graph_algorithm_results` exist after
  migration.
- `src/db/mod.rs::migrate_is_idempotent` verifies a second pass applies no new
  migrations and skips all previously applied versions.
- `src/db/mod.rs::apply_migration_is_idempotent_under_recheck` verifies the
  inner race recheck returns `AlreadyApplied` and preserves the original
  migration record.
- `src/db/mod.rs::graph_algorithm_witnesses_write_and_read_back` and
  `graph_algorithm_witnesses_filter_by_snapshot_and_algorithm` verify witness
  persistence and lookup behavior.
- `src/db/mod.rs::graph_algorithm_results_upsert_and_read_back`,
  `graph_algorithm_results_filter_by_snapshot_and_algorithm`, and
  `graph_algorithm_results_evicts_snapshots_older_than_latest` verify cache
  upsert, lookup, and stale-snapshot eviction behavior.
- `tests/contracts/schema_drift.rs` freezes the live table, index, and column
  inventory for both graph algorithm tables.

When the shared graph compile blockers are clear, the focused RCH check for
this surface is:

```bash
cargo test --test graph_migrations -- --nocapture
cargo test --lib graph_algorithm -- --nocapture
cargo test --test ppr_context_pack -- --nocapture
cargo test --test contracts schema_drift -- --nocapture
```

Run those through `rch`; do not run local Cargo on the Mac checkout.
