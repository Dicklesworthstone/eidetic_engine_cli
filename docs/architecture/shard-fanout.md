# Per-Workspace Shard Fan-Out

This document is the architecture contract for `bd-f6jfs`: replacing global
file-backed write serialization with per-workspace FrankenSQLite shards while
preserving deterministic retrieval, audit provenance, and no-delete recovery.

The controlling ADR is [ADR 0040](../adr/0040-per-workspace-shard-fanout.md).

## Current Anchors

- `src/db/mod.rs` defines per-database write-owner gates through
  `FILE_WRITE_OWNER_GATES` near the top of the storage module.
- `src/db/mod.rs` acquires that gate in `lock_file_write_owner_gate` around
  lines 308-340.
- `src/db/mod.rs` reaches the gate from `DbConnection::open_once` for
  read-write file databases around lines 507-515.
- `src/db/mod.rs` keeps manual transaction warnings around lines 586-615;
  downstream code must preserve safe transactional write helpers.
- `src/core/workspace.rs` exposes deterministic workspace identity and registry
  reports through `WorkspaceEntry` and `WorkspaceResolveReport`.
- `src/config/env_registry.rs` is the required registry for any new `EE_*`
  variables.
- `src/core/search.rs` currently defaults search database paths to
  `<workspace>/.ee/ee.db` and carries memory-scope and strict-scope options.
- `src/core/memory_scope.rs` owns workspace/team/swarm scope and mesh
  authorization/redaction decisions.
- `src/core/backup.rs` currently creates backups from one workspace database
  path and optional derived assets.
- `src/cli/mod.rs` exposes current `ee backup`, `ee db`, and `ee migrate`
  surfaces.

## Layout

Default data root:

```text
~/.local/share/ee/
  catalog.db
  shards/
    <workspace_id>.db
```

Workspace-local override for tests or explicit operators:

```text
EE_SHARDS_DIR=/path/to/shards
```

Feature gate:

```text
EE_SHARD_FANOUT_ENABLED=1
```

Status-only resolver contract:

- When `EE_SHARD_FANOUT_ENABLED` is absent or false, `ee status --json` and
  `ee doctor --json` report shard fan-out as `disabled` and keep
  `<workspace>/.ee/ee.db` authoritative.
- When enabled, the read-only resolver derives a shard path from the stable
  workspace ID: `<shards_dir>/<workspace_id>.db`.
- The default shard directory is `$XDG_DATA_HOME/ee/shards` when
  `XDG_DATA_HOME` is set, otherwise `$HOME/.local/share/ee/shards`.
- `EE_SHARDS_DIR` overrides only the shard directory. The catalog is planned as
  the sibling `catalog.db` under the same data root.
- Resolver/status/doctor inspection must not create, delete, migrate, or open
  files for writing. Missing catalog or shard files are reported as
  `migration_required`.
- The shard directory must be absolute and must not contain `..` or existing
  symlinked path components. Unsafe roots fail closed with
  `shard_fanout_root_unsafe`.

The catalog records:

- layout schema version;
- workspace ID to shard ID/path mapping;
- workspace alias and canonical path metadata needed for routing;
- migration phase and last verified hash per shard;
- rollback availability and preserved legacy DB path;
- global posture fields for status, doctor, support bundles, and closeout.

Each shard records one workspace's source-of-truth rows. A shard should contain
memories, audit rows, pack records, mesh imported rows, and any workspace-local
tables required by command execution. Derived assets may have their own files,
but they remain rebuildable from the shard plus catalog.

## Write Routing

The router takes a workspace identity and returns one shard handle:

```text
workspace path -> workspace_id -> catalog lookup -> shard path -> DbConnection
```

The storage-layer router is `src/db/shard.rs::DbShardRouter`. It wraps the
status-only resolver from the previous slice and chooses either:

- `routingMode=legacy`, with `<workspace>/.ee/ee.db` as the database path when
  `EE_SHARD_FANOUT_ENABLED` is false; or
- `routingMode=shard_fanout`, with `<shards_dir>/<workspace_id>.db` only when
  the resolver reports `posture=enabled`.

Enabled-but-missing catalog or shard files are not opened through the router;
they remain `migration_required` until the migration bead materializes and
verifies the layout.

Write rules:

1. One durable write command targets exactly one workspace shard.
2. The per-shard `.write.lock` remains the cross-process lock for that file.
3. Same-shard writes serialize and preserve one audit order.
4. Different-shard writes must not share the old process-wide mutex.
5. Catalog writes stay rare and must not be on the foreground write hot path
   unless the command is creating, migrating, repairing, or deleting no data.

Inside one process, `src/db/mod.rs` now keeps one write-owner mutex per database
location instead of one process-wide mutex for every file database. Re-entrant
same-file writes track depth per database path; a different shard file receives
an independent process gate and still takes its own `.write.lock`. Nested
attempts to hold two database-file write owners on the same thread fail closed
rather than creating an implicit cross-shard write transaction.

The router must treat symlink and unsafe-path checks as source-of-truth safety
concerns. A shard path that escapes the configured data root or traverses a
symlinked component must fail closed.

## Read Routing

Single-workspace reads open one shard, preferably through the read-pool surfaces
once the router exists.

Cross-workspace reads are read-only. Search, context, why, audit timeline, and
inspection commands may attach or open peer shards only in read-only mode.

Read union rules:

- Apply memory-scope and mesh policy before including peer rows.
- Redact or omit peer bodies according to the policy decision.
- Order results deterministically by score, timestamp, workspace ID, shard ID,
  and stable row ID tie-breakers.
- Emit degraded codes when a peer shard cannot be attached, but do not fail a
  local-only result set unless strict scope requires the missing peer.
- Never use a read-only peer attachment as permission to write into that peer.

## Audit Strategy

Audit chains are per shard.

Required shard audit fields:

- `workspace_id`
- `shard_id`
- local audit sequence or row ID
- previous local audit hash
- current local audit hash
- operation/action
- subject ID
- actor
- timestamp

The global audit timeline is a view:

```text
UNION ALL per-shard audit rows
ORDER BY audit_ts, workspace_id, shard_id, audit_id
```

That global view is for operator comprehension. It is not a cross-shard commit
chain and must not be described as one. A row in the global timeline must expose
the shard that produced it so `ee why`, support bundles, and backup verification
can explain provenance.

## Migration

Target command:

```bash
ee migrate shard-fanout --workspace . --dry-run --json
ee migrate shard-fanout --workspace . --json
```

Dry-run must report:

- source `ee.db` path;
- catalog path;
- shard path for each workspace;
- row counts per table/workspace;
- source DB hash;
- target DB hash if materialized in a temp plan;
- preserved legacy DB path;
- migration phase;
- degraded codes;
- recovery actions.

Apply must be deterministic:

1. Inspect and hash the source DB.
2. Build the catalog and shard plan.
3. Copy rows into per-workspace shards.
4. Verify row counts and hashes.
5. Preserve original `ee.db` as `.pre-shard-fanout.db` or another documented
   no-delete path.
6. Emit `ee.migration.shard_fanout.v1` audit events.
7. Mark the shard layout authoritative only after verification succeeds.

Second run must be idempotent. Interrupted or corrupt migrations must leave a
clear phase and recovery action.

## Rollback And Off Switch

`EE_SHARD_FANOUT_ENABLED=0` means legacy layout behavior. It is not a silent
dual-write mode.

Rollback must:

- start with dry-run;
- report all paths, hashes, row counts, and safety checks;
- use the preserved legacy DB or verified backup as source;
- restore behavior into an isolated or explicitly approved target;
- never delete shard files or preserved DB evidence.

Fail-closed examples:

- catalog exists but a required shard is missing;
- catalog points outside the allowed shard root;
- disabled mode sees only a migrated layout and no preserved legacy DB;
- a command requests a cross-shard write transaction;
- shard audit-chain verification fails.

## Backup And Restore

Backup create must include:

- `catalog.db`;
- every workspace shard needed by the selected scope;
- manifest entries with workspace ID, shard ID, relative path, size, hash,
  schema version, and redaction posture;
- derived asset manifests when explicitly requested.

Restore must reconstruct the same layout under a side path and verify hashes
before reporting success. Restore must not overwrite a live catalog or shard
unless a later operator-approved workflow defines that behavior.

## Status, Doctor, And Support Bundles

Status and diagnostic surfaces should report:

- feature enabled/disabled;
- catalog path and existence;
- shard root;
- shard count;
- current migration phase;
- rollback availability;
- per-shard audit-chain posture;
- degraded codes and structured recovery actions.

Posture must distinguish `ok`, `initializing`, `degraded_recoverable`,
`degraded_required`, and `blocked` in the same spirit as the existing response
contract.

## Tracing And Test Events

Shard fan-out tracing uses:

- `surface=shard_fanout`
- `workspace_id`
- `shard_id`
- `request_id`
- `phase`
- `elapsed_ms`
- `degraded_codes`

Allowed phases:

- `input`
- `catalog_lookup`
- `attach_peer`
- `write`
- `chain_append`
- `cross_shard_union`
- `response`

The concurrency harness in `bd-f6jfs.8` must additionally emit
`ee.test_event.v1` events with fields for:

- `workspace_id`
- `shard_id`
- `phase=enqueue|grant|commit`
- `elapsed_ms`
- `worker_id` or equivalent
- `request_id` or operation ID

## Degraded Codes

Downstream implementation beads should add failure-mode fixtures and taxonomy
entries for codes such as:

- `shard_attach_failed`
- `shard_chain_mismatch`
- `cross_shard_skew_detected`
- `shard_catalog_missing`
- `shard_catalog_mismatch`
- `shard_missing`
- `shard_unsafe_path`
- `shard_fanout_disabled_migrated_layout`
- `cross_shard_write_unsupported`

The exact code names may change during implementation, but every emitted code
must have a fixture under `tests/fixtures/failure_modes/`, taxonomy coverage,
and structured recovery actions.

## Implementation Order

The child beads intentionally keep the rollout narrow:

1. `bd-f6jfs.1`: ADR, architecture doc, and migration schema.
2. `bd-f6jfs.2`: catalog, shard path resolver, and env registry.
3. `bd-f6jfs.3`: DbShardRouter and per-shard write ownership.
4. `bd-f6jfs.4`: deterministic migration and preserved rollback path.
5. `bd-f6jfs.5`: cross-shard read attach and search parity.
6. `bd-f6jfs.6`: per-shard audit chains and global timeline view.
7. `bd-f6jfs.7`: backup, restore, and side-path parity for shards.
8. `bd-f6jfs.8`: concurrency e2e and throughput benchmark harness.
9. `bd-f6jfs.9`: rollback, off-switch, and fail-closed behavior.
10. `bd-f6jfs.10`: closeout proof matrix and RCH verification aggregation.

Do not skip directly to code that changes the write hot path without the
catalog, migration, rollback, and proof surfaces being explicit.

## Verification Checklist

For the docs/schema bead:

```bash
jq empty docs/schemas/ee.migration.shard_fanout.v1.json
git diff --check -- docs/adr/0040-per-workspace-shard-fanout.md docs/architecture/shard-fanout.md docs/schemas/ee.migration.shard_fanout.v1.json docs/adr/README.md
rg "catalog.db|shards/<workspace_id>.db|EE_SHARD_FANOUT_ENABLED|EE_SHARDS_DIR|shard_attach_failed|cross_shard_write_unsupported" docs/architecture/shard-fanout.md docs/adr/0040-per-workspace-shard-fanout.md
```

For implementation beads, Cargo verification is RCH-only. No local Cargo
fallback is acceptable on this Mac.
