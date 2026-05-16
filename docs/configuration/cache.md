# Cache Configuration

`ee` cache layers are derived assets. They may improve latency, but the durable
source of truth remains FrankenSQLite plus rebuildable search and graph indexes.
Any cache hit must preserve the same output contract as a fresh command.

This page defines the L2 pack cache configuration contract for `ee context`
swarm workloads. Runtime lookup, writes, and eviction land in later beads.

## L2 Pack Cache

The L2 pack cache is a host-local, cross-process cache for complete context
pack JSON. It is designed for a single large swarm host where many agents ask
similar or identical context questions against the same workspace.

Default directories:

- Linux: `/dev/shm/ee/packs/<workspace_id>/`
- macOS and other non-Linux systems: `$TMPDIR/ee_packs_l2/<workspace_id>/`
- Explicit override: `EE_L2_PACK_CACHE_DIR`

The cache directory must be created with mode `0700`. If the directory cannot
be created or written, `ee context` must continue through the normal assembly
path and emit a low-severity degraded signal rather than failing the request.

## Canonical Key

The L2 key is a BLAKE3 hash of every input that can affect emitted pack JSON.
The canonical key schema is `ee.pack.l2_cache_key.v1`.
At minimum, the canonical input set includes:

- workspace ID
- database generation
- index generation
- graph generation when graph-derived fields can affect output
- redaction level
- context profile
- resource profile
- max token budget
- candidate pool
- memory scope
- strict-scope mode
- normalized query
- context feature flag set hash
- profile or personalization generation when per-agent profile bias applies

Same canonical input set means same key. Any changed input that can change JSON
must change the key. Do not serve stale data and rely on downstream consumers
to notice.

## Cached Value

The cache value is the exact successful `ee context --json` response body. It
is not wrapped in cache metadata. A cache hit should be byte-for-byte equivalent
to a fresh assembly result for the same canonical key.

Operational metadata belongs in tracing events, not inside the cached JSON
body. This keeps cached and fresh responses comparable by ordinary determinism
tests.

## Read Path

The intended read order is:

1. Validate request and compute the canonical key.
2. Check the existing in-process L1 cache.
3. Check the host-local L2 cache.
4. Assemble a fresh pack on miss.
5. Write the successful response body to L2 after assembly.

L2 reads must be lock-free. Writers publish entries with a temporary file plus
atomic rename. Readers should see either a complete JSON file or no file.

## Write And Eviction

Writes should be best-effort. A failed write must not fail the command.

Eviction is lazy and write-triggered. The default maximum size is 1 GiB per
workspace unless `EE_L2_PACK_CACHE_BYTES` overrides it. When the cap is
exceeded, evict least-recently-used entries by access time or the closest
portable approximation available on the host filesystem.

Eviction must not block the read path. If eviction fails, emit a degraded
signal and leave correctness unchanged.

## Failure Modes

Expected degraded codes:

- `l2_pack_cache_unavailable`: cache directory is missing, unwritable, or
  disabled by configuration.
- `l2_pack_cache_corruption`: an entry exists but is not valid JSON or does not
  match the expected response shape.

Both are response-time degradations. They should be low severity because normal
pack assembly remains available.

On corruption, invalidate only the corrupted entry and assemble fresh output.
Do not delete broader cache directories as part of request handling.

## Privacy

The L2 cache stores final emitted JSON, so it inherits the caller's redaction
level. Redaction level must be part of the canonical key. A redacted request and
an unredacted request must never share the same cache entry.

The cache directory mode must be `0700` to keep host-local agent artifacts out
of other users' accounts. Support bundles should report cache health and size,
not cached pack bodies.

## Configuration Keys

Config shape:

```toml
[cache.pack_l2]
enabled = true
directory = ""
max_bytes = 1073741824
max_age_days = 30
```

Environment variables:

- `EE_L2_PACK_CACHE_DISABLE`: disables L2 lookup and writes when `true`.
- `EE_L2_PACK_CACHE_DIR`: overrides the root cache directory.
- `EE_L2_PACK_CACHE_BYTES`: overrides the per-workspace byte cap.

All `EE_*` variables are registered in `src/config/env_registry.rs`.

## Tracing

Required tracing fields for L2 cache events:

- `workspace_id`
- `request_id`
- `surface=pack_cache_l2`
- `phase=lookup|hit|miss|write|evict|corruption|unavailable`
- `elapsed_ms`
- `degraded_codes`
- `l2_key_prefix`
- `cache_bytes`
- `cache_entries`

`l2_key_prefix` should be a short redaction-safe prefix, not the full canonical
input set.

## Validation Checklist

- Same canonical inputs produce the same key across processes.
- Different redaction levels produce different keys.
- Different DB/index/graph generations produce different keys.
- L2 hit body equals fresh batch JSON byte-for-byte.
- Corrupted entry falls through to fresh assembly and records degradation.
- Unwritable cache directory records degradation and does not fail context.
- Cache directory is created with mode `0700`.
- Eviction respects the configured per-workspace size cap.
- Four concurrent identical context requests produce one fresh assembly and
  three L2 hits in the e2e harness.
