# SQLModel FrankenSQLite Repository Shape Spike

Bead: `eidetic_engine_cli-9trk` / EE-280

## Recommendation

**Go, with guardrails.** `ee` should use SQLModel Rust's core/model/query/schema
layers with the FrankenSQLite driver, but the first storage slice should keep a
thin `ee-db` repository boundary instead of exposing SQLModel or FrankenSQLite
types to CLI/core callers.

The first implementation should prove this path with a small contract test
before building broad storage features:

```text
typed DB records -> static migration -> FrankenConnection
                 -> repository insert/query -> domain model conversion
                 -> stable status/why/context JSON metadata
```

## Sources Read

| Source | Observed Facts |
| --- | --- |
| `/data/projects/sqlmodel_rust` at `9192b11` | Workspace version `0.2.2`; includes `sqlmodel-frankensqlite`; SQLModel core uses `Cx` and `Outcome`; facade default features are empty. |
| `/data/projects/frankensqlite` at `9034b2e9` | Public `fsqlite` version is `0.1.2`; runtime is currently hybrid/pager-backed; extension crates are feature-gated; compatibility gaps are documented. |

Primary references:

- `/data/projects/sqlmodel_rust/README.md:28` through `/data/projects/sqlmodel_rust/README.md:34`
- `/data/projects/sqlmodel_rust/README.md:86` through `/data/projects/sqlmodel_rust/README.md:106`
- `/data/projects/sqlmodel_rust/Cargo.toml:56` through `/data/projects/sqlmodel_rust/Cargo.toml:87`
- `/data/projects/sqlmodel_rust/crates/sqlmodel-core/src/connection.rs:132` through `/data/projects/sqlmodel_rust/crates/sqlmodel-core/src/connection.rs:227`
- `/data/projects/sqlmodel_rust/crates/sqlmodel-frankensqlite/src/connection.rs:1` through `/data/projects/sqlmodel_rust/crates/sqlmodel-frankensqlite/src/connection.rs:49`
- `/data/projects/sqlmodel_rust/crates/sqlmodel-frankensqlite/src/connection.rs:63` through `/data/projects/sqlmodel_rust/crates/sqlmodel-frankensqlite/src/connection.rs:103`
- `/data/projects/sqlmodel_rust/crates/sqlmodel-frankensqlite/src/connection.rs:268` through `/data/projects/sqlmodel_rust/crates/sqlmodel-frankensqlite/src/connection.rs:346`
- `/data/projects/sqlmodel_rust/crates/sqlmodel-schema/src/migrate.rs:337` through `/data/projects/sqlmodel_rust/crates/sqlmodel-schema/src/migrate.rs:492`
- `/data/projects/frankensqlite/README.md:20` through `/data/projects/frankensqlite/README.md:30`
- `/data/projects/frankensqlite/README.md:145` through `/data/projects/frankensqlite/README.md:152`
- `/data/projects/frankensqlite/crates/fsqlite/Cargo.toml:10` through `/data/projects/frankensqlite/crates/fsqlite/Cargo.toml:45`

## Dependency Shape

Use the SQLModel facade for application code and explicit sub-crates where
`ee-db` needs lower-level types. Do not enable the C SQLite test driver.

Target dependency intent:

```toml
sqlmodel = { version = "0.2.2", default-features = false }
sqlmodel-core = { version = "0.2.2" }
sqlmodel-query = { version = "0.2.2" }
sqlmodel-schema = { version = "0.2.2" }
sqlmodel-session = { version = "0.2.2" }
sqlmodel-pool = { version = "0.2.2" }
sqlmodel-frankensqlite = { version = "0.2.2" }

fsqlite = { version = "0.1.2", default-features = false, features = ["native", "json", "fts5"] }
fsqlite-core = { version = "0.1.2", default-features = false }
fsqlite-types = { version = "0.1.2", default-features = false }
fsqlite-error = { version = "0.1.2" }
```

Two caveats must be resolved by the implementation bead:

- `sqlmodel-frankensqlite` currently depends on `fsqlite = "0.1.2"` without
  `default-features = false`, so `ee` may need an upstream feature patch before
  it can precisely own the fsqlite feature set.
- `fsqlite` has a `rusqlite` dev-dependency for parity tests. That is not a
  normal runtime dependency, but `ee` must prove its own default and all-target
  dependency tree excludes `rusqlite`.

## Repository Boundary

Keep this shape inside the current single-crate Phase 0 module layout:

```text
src/db/
  mod.rs              # public DB facade for core
  connection.rs       # open/close, schema-only open, path policy
  migrations.rs       # static EE migrations and migration status
  records.rs          # SQLModel record structs
  repositories.rs     # MemoryRepository, PackRepository, WorkspaceRepository
  errors.rs           # storage error mapping to ee error codes
```

`core` should see repository traits and domain models, not SQLModel query
builders or FrankenSQLite rows. CLI code should never depend on `db`.

Recommended public boundary:

```rust
pub struct Db {
    conn: sqlmodel_frankensqlite::FrankenConnection,
}

pub struct Repositories<'db> {
    pub workspaces: WorkspaceRepository<'db>,
    pub memories: MemoryRepository<'db>,
    pub packs: PackRepository<'db>,
    pub migrations: MigrationRepository<'db>,
}
```

Repository methods should return domain types from `models`, not output structs.
Conversion belongs at the `db` boundary:

```text
MemoryRecord <-> Memory
WorkspaceRecord <-> Workspace
PackRecord <-> ContextPackRecord
MigrationRecord <-> MigrationStatus
```

## Initial Record Set

The walking skeleton only needs a small durable model set:

| Record | Purpose |
| --- | --- |
| `WorkspaceRecord` | Stable workspace identity and path fingerprint. |
| `MemoryRecord` | Manual memory text, level, kind, timestamps, confidence, content hash. |
| `MemoryProvenanceRecord` | Evidence pointer for explicit notes and later CASS spans. |
| `PackRecord` | Persisted context pack request, selected memory IDs, hash, token budget. |
| `SchemaMigrationRecord` | Static migration ID, description, applied timestamp or test-controlled clock value. |
| `CapabilityRecord` | Stored degraded capability snapshot for `ee status --json` where useful. |

Store public IDs as text for stable JSON and debugging. Store content hashes as
hex text unless benchmarks later justify BLOBs. Expose RFC 3339 timestamps at
the output layer, but prefer integer epoch micros or millis inside the DB.

## Connection Policy

Use one FrankenSQLite connection per CLI command in the first slice. This keeps
the implementation local-first and avoids hidden daemon requirements.

Use `FrankenConnection::open_schema_only` for read-only status/doctor/schema
inspection paths that must avoid writer semantics. Use normal file open for
commands that may apply migrations or write memory.

Do not share a single connection as the multi-agent write-concurrency solution.
The current adapter makes `FrankenConnection` `Send + Sync` by wrapping the
underlying `!Send` `fsqlite::Connection` in `Arc<Mutex<_>>`, so sharing it
serializes operations inside one process. Future multi-agent write posture still
needs the advisory lock/daemon-owner contract.

## Migration Policy

Use static migration IDs. Do not call `Migration::new_version()` for shipped
migrations because it derives IDs from wall-clock time.

Acceptable initial shape:

```rust
pub const INITIAL_SCHEMA_ID: &str = "0001_initial";

pub fn migrations() -> Vec<sqlmodel_schema::Migration> {
    vec![sqlmodel_schema::Migration::new(
        INITIAL_SCHEMA_ID,
        "initial walking skeleton schema",
        include_str!("migrations/0001_initial.up.sql"),
        include_str!("migrations/0001_initial.down.sql"),
    )]
}
```

The existing `MigrationRunner` can track `_ee_migrations` because it supports a
custom sanitized table name. However, it writes `applied_at` using wall-clock
time. Either hide `applied_at` from stable JSON output, make tests use a fixed
fixture DB, or patch SQLModel upstream to inject a clock before relying on that
value in golden tests.

Migration application should run inside an explicit write path:

```text
open DB -> acquire EE write lock -> init migration table -> apply static migrations
        -> write audit/status metadata -> release write lock
```

## Transaction Policy

For the first slice, use ordinary SQLModel transactions. The current
FrankenSQLite adapter maps SQLModel isolation levels to:

| SQLModel Isolation | FrankenSQLite SQL |
| --- | --- |
| `Serializable` | `BEGIN EXCLUSIVE` |
| `RepeatableRead` | `BEGIN IMMEDIATE` |
| `ReadCommitted` | `BEGIN IMMEDIATE` |
| `ReadUncommitted` | `BEGIN DEFERRED` |

Do not claim `BEGIN CONCURRENT` behavior through the repository boundary until a
dedicated contract test proves it. FrankenSQLite supports `BEGIN CONCURRENT`,
but the SQLModel adapter does not currently select it from `IsolationLevel`.

## Cancellation And Budget Caveat

SQLModel's `Connection` trait is shaped correctly: async methods take `&Cx` and
return `Outcome`. The current FrankenSQLite driver, however, runs synchronous
work before returning an immediately-ready future and names the context `_cx` in
query/execute/begin/prepare paths.

That is acceptable only for short foundation-slice operations. Long imports,
index rebuilds, bulk curation, and repair jobs need one of:

- upstream driver support for cooperative `Cx` checks
- explicit chunking around repository calls
- Asupersync supervised blocking boundaries with clear cancellation behavior

Gate tests should prove cancellation at the command path even if a single small
SQLite call is not interruptible.

## Query And Schema Policy

Use SQLModel derive records for tables, SQLModel query builders for simple CRUD,
and raw SQL only at documented boundaries:

- migrations
- PRAGMAs/status inspection
- FTS5 index setup or search maintenance
- narrow compatibility workarounds with tests

Keep search indexes derived assets. The DB records should capture canonical
memory/source/pack state; Frankensearch indexing should consume repository
records rather than becoming the source of truth.

## Required Contract Tests

Gate 2 should include:

1. Open `:memory:` and file-backed `FrankenConnection`.
2. Apply a static migration set to `_ee_migrations`.
3. Re-run migrations and prove idempotent status.
4. Insert and query a workspace, manual memory, provenance row, and pack record.
5. Convert DB records to domain models and stable JSON-friendly data.
6. Open schema-only status path without applying writes.
7. Simulate migration-required and unsupported-future-schema states.
8. Run a dependency audit proving no `tokio`, `rusqlite`, `sqlx`, `diesel`,
   `sea-orm`, or `petgraph` appears in the `ee` dependency tree.
9. Record a closure dossier with fixture IDs and artifact paths once the
   fixture manifest exists.

## Gotchas

- **Unsafe wrappers live upstream.** `sqlmodel-frankensqlite` uses `unsafe impl`
  for `Send`/`Sync` around a mutex-guarded `fsqlite::Connection`. Keep `ee`
  itself under `#![forbid(unsafe_code)]`; do not copy that wrapper locally.
- **Feature trimming is not solved by `ee` alone.** The adapter currently pulls
  `fsqlite` with its default feature set. A dependency-contract bead must prove
  the resulting tree is acceptable or patch upstream.
- **Migration timestamps are nondeterministic.** Static migration IDs are
  required; `applied_at` must be controlled, hidden from stable output, or
  patched upstream for golden tests.
- **`Cx` is not yet operational in the driver hot path.** Treat repository calls
  as short blocking sections until upstream cancellation support lands.
- **FrankenSQLite is real but still hybrid.** The README documents fallback
  paths and incomplete extension wiring. `ee` should test the exact SQL it uses
  instead of assuming full SQLite parity.
- **Do not enable `sqlmodel-sqlite`.** The SQLModel facade has default features
  off; keep them off so the C SQLite driver and `libsqlite3-sys` stay out of
  normal `ee` storage.

## Closeout

This spike is documentation-only. It does not add executable tests because the
follow-up storage contract and migration beads own implementation proof:

- `eidetic_engine_cli-v00k` / Gate 2: SQLModel Plus FrankenSQLite Contract Test
- `eidetic_engine_cli-gyml` / Gate 0: Integration Foundation Smoke Test
- `eidetic_engine_cli-q9f` / EE-040: Wire SQLModel FrankenSQLite connection
- `eidetic_engine_cli-tx6f` / EE-041: Implement migration table
- `eidetic_engine_cli-koat` / EE-042: Create initial migration
