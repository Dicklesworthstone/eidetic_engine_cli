# ADR 0040: Per-Workspace Shard Fan-Out

Status: proposed
Date: 2026-05-19
Bead: bd-f6jfs.1
Builds on: ADR 0002 (FrankenSQLite + SQLModel Source Of Truth), ADR 0013 (Single Write Owner Actor), ADR 0017 (Swarm-Scale Resource Governance), ADR 0039 (Write-Hot-Path V2 Implementation Gate)

## Context

`ee` currently opens each file-backed read-write database through a process-wide
write-owner gate. The current source anchor is `src/db/mod.rs`: the singleton
`FILE_WRITE_OWNER_GATE` is defined around lines 278-282, acquired by
`lock_file_write_owner_gate` around lines 308-340, and reached during
`DbConnection::open_once` for read-write file databases around lines 507-515.
The file-level `.write.lock` remains valuable, but the process-wide mutex means
unrelated workspaces on the same host can serialize behind each other before
FrankenSQLite sees the request.

That shape is acceptable for one local agent. It becomes a tail-latency problem
on 64+-core / 256GB+ swarm hosts where many agents use independent workspaces
but share one `ee` binary and one user data root. The parent bead `bd-f6jfs`
targets that specific bottleneck: workspace A writes should not block workspace
B writes when the two workspaces have disjoint source-of-truth databases.

This decision does not repeal the single-write-owner requirement inside one
durable write domain. It narrows the write domain from "all file-backed ee
databases in this process" to "one workspace shard file." Same-shard writes
remain serialized and auditable.

## Decision

`ee` will introduce an opt-in per-workspace shard layout:

```text
~/.local/share/ee/
  catalog.db
  shards/
    <workspace_id>.db
```

The catalog is a thin coordinator. It owns workspace-to-shard mapping, alias
metadata, shard layout version, migration state, and global posture fields. It
is intentionally low-write. A workspace's memories, audit rows, derived-source
metadata, pack records, mesh imports, and search/index source rows live in that
workspace's shard unless a later ADR explicitly defines a global table.

The v1 routing rule is strict:

1. A write for workspace `W` opens and mutates only shard
   `shards/<workspace_id>.db`.
2. The per-file `.write.lock` remains the cross-process guard for that shard.
3. The process-wide singleton write gate must not serialize different shard
   files.
4. Same-shard writes still have one observable durable order and one local audit
   chain.
5. Cross-shard transactions are not part of v1.

Read-only cross-workspace features, including peer search and context assembly,
may attach or open peer shards read-only and union deterministic result sets.
Those reads must still honor `src/core/memory_scope.rs` policy, including mesh
peer authorization, redaction, and strict-scope behavior.

The feature is gated by configuration:

- `EE_SHARD_FANOUT_ENABLED` selects the new layout.
- `EE_SHARDS_DIR` overrides the default shard directory for explicit tests or
  operator-controlled data roots.

Both variables must be registered through `src/config/env_registry.rs`; raw
`std::env::var("EE_*")` call sites remain forbidden.

## Invariants

### I1: Workspace identity is the shard key

The shard key is the deterministic workspace ID, not a mutable path string,
agent name, hash bucket, or memory kind. `src/core/workspace.rs` already exposes
workspace reports with `workspace_id`, canonical paths, aliases, and repository
scope fields. Shard routing must derive from that identity surface.

### I2: One authoritative write layout at a time

An enabled process writes to the shard layout. A disabled process writes to the
legacy single database layout. It is invalid to silently dual-write or to accept
some writes into `ee.db` and others into shards for the same workspace state.
Ambiguous layout state must fail closed with structured recovery actions.

### I3: No cross-shard transactions in v1

V1 rejects cross-shard write transactions. A command that cannot be represented
as a single-workspace write plus read-only peer inspection must return a typed
error or degraded response. Reintroducing cross-shard commit coordination would
also reintroduce a global serialization point and requires a separate ADR.

### I4: Migration preserves the original database

`ee migrate shard-fanout` must be deterministic and no-delete. The original
`ee.db` is preserved as evidence, for example as `.pre-shard-fanout.db`, before
the shard layout is treated as authoritative. Rollback uses that preserved file
or a verified backup; it must not delete source data.

### I5: Audit chains are local and explainable

Every shard has its own contiguous audit-chain sequence. The global audit
timeline is a deterministic read-only union sorted by audit timestamp plus
stable tie-breakers. A global timeline row must expose which shard produced it.
Cross-shard ordering is explanatory, not a cross-shard commit proof.

### I6: Search indexes and graph snapshots remain derived

FrankenSQLite/SQLModel remain the source of truth. Frankensearch indexes, graph
snapshots, pack caches, and other derived assets are rebuildable from catalog
and shard files. A stale or missing derived asset must not make a shard write
silently disappear.

### I7: RCH-only verification for Rust gates

Docs and JSON schemas may use local static checks such as `jq` and
`git diff --check`. Cargo builds, tests, clippy, benches, and broad verify
commands must run through RCH only on this Mac. If RCH is unavailable, the
implementation bead remains open with exact blocker evidence.

## Consequences

Different workspaces can make foreground write progress independently on the
same host. The file-level lock still protects cross-process access to one shard,
and the per-shard audit chain keeps provenance reviewable.

The cost is routing complexity. Commands that previously accepted a single
database path must know whether they are addressing the catalog, one shard, or a
read-only set of peer shards. Status, doctor, backup, restore, and support
bundle surfaces must report layout posture explicitly so agents do not infer the
wrong source of truth.

Migration and rollback become first-class product surfaces, not implementation
details. Operators must be able to see the catalog path, shard count, migration
phase, preserved legacy DB path, hashes, and recovery actions before trusting a
layout transition.

## Rejected Alternatives

- **Cross-shard transactions in v1.** They make the design harder to reason
  about and recreate a global commit point. V1 chooses single-workspace writes
  plus read-only peer inspection.
- **Fixed hash shards.** Hashing memories into N buckets breaks the
  workspace-as-truth-unit model and complicates backup, restore, audit review,
  and peer authorization.
- **Per-table writers.** Multiple table-specific write orders fragment the
  audit chain and conflict with ADR 0039's write-hot-path correctness gate.
- **Monolithic single-DB tuning only.** WAL settings, read pools, and faster
  locks help, but they cannot let independent workspaces commit concurrently if
  every write enters the same process-wide gate.
- **Silent compatibility shim.** Running old and new write layouts at once hides
  data ownership. Compatibility must be explicit: enabled, disabled, migrated,
  rollback-available, or blocked.

## Verification

This ADR is a contract for the downstream `bd-f6jfs.*` implementation beads.
The final closeout must attach evidence for:

1. `docs/architecture/shard-fanout.md` describes catalog, shard files,
   read-only peer attachment, no cross-shard transactions, audit chains,
   migration/rollback, and backup/restore.
2. `docs/schemas/ee.migration.shard_fanout.v1.json` validates with `jq` and
   includes `workspace_id`, `shard_id`, `source_db_hash`, `target_db_hash`,
   `migration_phase`, `dry_run`, `actor`, `elapsed_ms`, and `degraded_codes`.
3. `src/config/env_registry.rs` registers `EE_SHARD_FANOUT_ENABLED` and
   `EE_SHARDS_DIR` before production code reads them.
4. Same-shard writes serialize while different-shard writes do not share the
   old process-wide gate.
5. `ee migrate shard-fanout --dry-run --json` reports planned paths, hashes,
   row counts, and no-delete preservation before mutation.
6. Cross-shard search/context parity tests prove deterministic result ordering
   and preserve memory-scope policy.
7. Backup and restore include catalog plus all shard files and restore to an
   isolated side path.
8. Rollback/off-switch tests prove fail-closed behavior for partial, corrupt,
   disabled, and migrated layouts.
9. The concurrency harness records `ee.test_event.v1` events for enqueue,
   grant, and commit phases and proves the parent throughput target or records a
   current RCH blocker.

Static verification for this docs bead:

```text
jq empty docs/schemas/ee.migration.shard_fanout.v1.json
git diff --check -- docs/adr/0040-per-workspace-shard-fanout.md docs/architecture/shard-fanout.md docs/schemas/ee.migration.shard_fanout.v1.json docs/adr/README.md
rg "ADR 0040|ee.migration.shard_fanout.v1|EE_SHARD_FANOUT_ENABLED|EE_SHARDS_DIR|cross-shard transactions" docs/adr/0040-per-workspace-shard-fanout.md docs/architecture/shard-fanout.md docs/adr/README.md
```

No Rust behavior changes are required by this ADR.
