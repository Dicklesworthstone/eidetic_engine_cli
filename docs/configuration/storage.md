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
pin_snapshot = true
```

`database_path` is the FrankenSQLite source of truth. `index_dir` contains
derived search assets that can be rebuilt from storage.

`[storage.read_pool]` controls read-side connection reuse for read-heavy
surfaces. The default `size = 1` preserves the single-connection behavior until
callers opt into more concurrency. `idle_timeout_seconds` bounds how long idle
read handles stay open. `pin_snapshot = true` keeps multi-step reads on a
stable snapshot; set it to `false` only when a caller explicitly wants unpinned
read visibility. The acquire timeout defaults to 5000 ms in the read-pool
runtime and bounds how long a caller waits for a pooled read handle before `ee`
opens a one-shot ad-hoc read connection outside the pool.

Snapshot pins are explicit read transactions over pooled FrankenSQLite
connections. A clean pin release returns the connection to the LIFO idle pool.
If release fails, `ee` abandons that connection instead of returning a possibly
dirty transaction state to later readers; this is reported through the
`snapshot_release_failed` degradation family. The follow-up read-pool lifecycle
work tracks max pin duration, watchdog poisoning, and workspace close drain
timeouts so long-held snapshots cannot grow the WAL without a visible repair
path.

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
| `storage.read_pool.pin_snapshot` | `EE_READ_POOL_DISABLE_PIN` inverts this value |
