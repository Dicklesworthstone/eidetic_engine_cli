# Storage Configuration

Storage settings live under `[storage]` in `.ee/config.toml`.

```toml
[storage]
database_path = "~/.local/share/ee/ee.db"
index_dir = "~/.local/share/ee/indexes"
jsonl_export = false

[storage.read_pool]
size = 1
idle_timeout_seconds = 30
max_pin_duration_seconds = 30
pin_snapshot = true
```

`database_path` is the FrankenSQLite source of truth. `index_dir` contains
derived search assets that can be rebuilt from storage.

`[storage.read_pool]` controls read-side connection reuse for read-heavy
surfaces. The default `size = 1` preserves the single-connection behavior until
callers opt into more concurrency. `idle_timeout_seconds` bounds how long idle
read handles stay open. `max_pin_duration_seconds` bounds how long a read
snapshot may remain pinned before lifecycle checks report `snapshot_pin_expired`.
`pin_snapshot = true` keeps multi-step reads on a
stable snapshot; set it to `false` only when a caller explicitly wants unpinned
read visibility. The acquire timeout defaults to 5000 ms in the read-pool
runtime and bounds how long a caller waits for a pooled read handle before `ee`
opens a one-shot ad-hoc read connection outside the pool.

Snapshot pins are explicit read transactions over pooled FrankenSQLite
connections. A clean pin release returns the connection to the LIFO idle pool.
If release fails, `ee` abandons that connection instead of returning a possibly
dirty transaction state to later readers; this is reported through the
`snapshot_release_failed` degradation family and the in-process
`ReadConnectionPool` `release_failures` counter. Long-held pins are tracked
against `max_pin_duration_seconds`; expired or force-released pins surface
through `snapshot_pin_expired` and `snapshot_pin_force_released` so they cannot
grow the WAL without a visible repair path.

## Snapshot Isolation Semantics

The current `SnapshotPin` primitive is source-verified in the local adapter:
`src/db/mod.rs` implements `DbConnection::begin_read_snapshot()` as
`BEGIN DEFERRED` through `execute_read_snapshot_raw()`. The file-backed
`execute_read_snapshot_raw()` path retries transient SQLite contention but does
not take the process-local write-owner gate. Upstream,
`/dp/sqlmodel_rust/crates/sqlmodel-frankensqlite/src/connection.rs` forwards
`FrankenConnection::execute_raw()` to `fsqlite::Connection::execute()`, so the
pin is a normal FrankenSQLite read transaction, not a special `ee` lock.

With `pin_snapshot = true`, the first read after `BEGIN DEFERRED` fixes the
reader's view until `SnapshotPin::commit`, `SnapshotPin::rollback`, or drop-time
rollback releases it. Same-process writers may commit through separate
connections while an older pin continues to see the pre-commit view; a later
pin sees the committed state. With `pin_snapshot = false`, reads are unpinned
autocommit queries: a later query on the same handle may observe newer committed
rows. Use `false` only for surfaces whose contract is freshness-over-stability.

Raise `storage.read_pool.size` deliberately. Start at `1`, then try `2`, `4`,
and `8` while watching `data.read_pool.acquire_wait.p99_ns`,
`data.read_pool.ad_hoc_bypass_count`, `data.wal.bytes`, and
`data.read_pool.active_pins`. Higher pool sizes improve concurrent read fan-out
only when pins are released promptly; long-lived pins can keep old WAL frames
alive and should be treated as a lifecycle bug before increasing the pool again.

## Acquire Backpressure And Bypass

When all pooled read connections are active, new acquirers yield cooperatively
and wait until a pooled handle is released or `acquire_timeout_ms` elapses. On
timeout, the request still proceeds through an ad-hoc read connection that is
closed on drop instead of returned to the pool. This keeps agent-facing read
surfaces responsive under bursts while making contention visible.

`ee status --json` reports:

- `data.read_pool.ad_hoc_bypass_count`: number of timeout-driven ad-hoc reads
  in the current process.
- `data.read_pool.acquire_wait.samples`: number of acquire wait samples in the
  sliding window.
- `data.read_pool.acquire_wait.p50_ns` and `p99_ns`: wait latency percentiles.

Sustained ad-hoc bypasses or high p99 wait times mean the pool is undersized for
the workload. Increase `storage.read_pool.size` explicitly; `ee` does not grow
the pool automatically because automatic growth can hide writer starvation and
WAL checkpoint pressure.

## WAL Growth Observability

`ee status --json` also reports the current workspace WAL sidecar as
`data.wal.bytes` and `data.wal.frames`. The frame count is derived from the WAL
file size and SQLite page size without running a checkpoint, so status remains a
read-oriented inspection surface.

`EE_WAL_CHECKPOINT_BYTES_THRESHOLD` defaults to 67108864 bytes (64 MiB). When
`data.wal.bytes` exceeds that threshold, status reports
`wal_growth_exceeds_threshold` with a repair command pointing at the explicit
checkpoint path. If WAL growth is visible from a read-only deployment and no
checkpoint writer is identified, status also reports `wal_growth_no_writer`.
Long-lived snapshot pins can prevent truncation; use the read-pool pin counters
and snapshot-pin degraded codes to identify readers that are holding old WAL
pages.

Run `ee maintenance wal-checkpoint --workspace . --json` to checkpoint through
the explicit maintenance writer path. The default mode is `passive`, which
moves checkpointable frames back into the main database without requiring every
reader to release its snapshot. Use
`ee maintenance wal-checkpoint --workspace . --mode truncate --json` after
checking the read-pool counters when the WAL sidecar remains above the
threshold and no long-lived snapshot pin is active. `--dry-run` reports the same
WAL counters without running a checkpoint.

Daemon ownership is intentionally not wired in this slice: `ee daemon status`
reports foreground-supervisor availability, but no autonomous daemon checkpoint
job owns WAL cleanup yet. Until that steward job exists, the explicit
`ee maintenance wal-checkpoint` command is the supported writer path for WAL
growth repairs.

## Why LIFO

Idle read handles are reused as a LIFO stack. The most recently released
connection is usually warm in FrankenSQLite page-cache terms, so reusing it
first preserves locality for repeated `ee context` and `ee status` lookups.
FIFO would distribute reads more evenly, but for this read-heavy workload it
keeps fewer handles hot and makes low-latency bursts worse.

Environment overrides:

| Config key | Env var |
| --- | --- |
| `storage.read_pool.size` | `EE_READ_POOL_SIZE` |
| `storage.read_pool.idle_timeout_seconds` | `EE_READ_POOL_IDLE_TIMEOUT_S` |
| `storage.read_pool.max_pin_duration_seconds` | `EE_READ_POOL_MAX_PIN_SECONDS` |
| `storage.read_pool.pin_snapshot` | `EE_READ_POOL_DISABLE_PIN` inverts this value |
| acquire timeout runtime override | `EE_READ_POOL_ACQUIRE_TIMEOUT_MS` |
| WAL checkpoint warning threshold | `EE_WAL_CHECKPOINT_BYTES_THRESHOLD` |
