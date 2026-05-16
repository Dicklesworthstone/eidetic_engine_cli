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
read visibility.

Snapshot pins are explicit read transactions over pooled FrankenSQLite
connections. A clean pin release returns the connection to the LIFO idle pool.
If release fails, `ee` abandons that connection instead of returning a possibly
dirty transaction state to later readers; this is reported through the
`snapshot_release_failed` degradation family. The follow-up read-pool lifecycle
work tracks max pin duration, watchdog poisoning, and workspace close drain
timeouts so long-held snapshots cannot grow the WAL without a visible repair
path.

Environment overrides:

| Config key | Env var |
| --- | --- |
| `storage.read_pool.size` | `EE_READ_POOL_SIZE` |
| `storage.read_pool.idle_timeout_seconds` | `EE_READ_POOL_IDLE_TIMEOUT_S` |
| `storage.read_pool.pin_snapshot` | `EE_READ_POOL_DISABLE_PIN` inverts this value |
