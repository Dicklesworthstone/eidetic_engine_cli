# Cache Configuration

`ee` cache layers are derived assets. They may improve latency, but the durable
source of truth remains FrankenSQLite plus rebuildable search and graph indexes.
Any cache hit must preserve the same output contract as a fresh command.

This page defines the L2 pack cache configuration contract for canonical
`ee pack` swarm workloads. The soft-deprecated `ee context` alias bypasses L2
entirely.

Normal persisted packs always assemble and perform their pack-record and audit
writes. A successful eligible call can then populate L2. A matching `--read-only`
JSON pack, including `--read-only --explain-performance`, can consume that entry.
Read-only misses still assemble normally and never populate, repair, delete, or
update the modification time of cache entries. A corrupt entry remains present
and produces the existing typed corruption degradation.

This reuse requires an explicit `--as-of` timestamp and lexical-only retrieval;
the current time is never silently frozen. Config, focus state, a global store,
agent-specific state, explicit database paths, file-backed evidence, and live
code anchors conservatively bypass reuse. Platforms without the supported stable
filesystem identity also bypass it. These restrictions preserve current policy
and provenance checks rather than treating cache identity as authorization.

For a published index, admission checks its real status and hashes its complete
contents, including lexical segments. Each fingerprint is bounded to 64 MiB and
4096 entries; larger, unreadable, symlinked, or changing indexes bypass reuse.
The fingerprint is checked again before a hit or publication. This adds index
read work; `pack_l2_index_fingerprint` debug events report `bytes_read`,
`entry_count`, and `elapsed_us` so comparisons can include that cost. Cache-hit
latency should be compared with a fresh read-only pack for the same indexed
workspace, timestamp, query, and budget, not a cold-model invocation.

## L2 Pack Cache

The L2 pack cache is a host-local, cross-process cache for complete context
pack JSON. It is designed for a single large swarm host where many agents ask
similar or identical context questions against the same workspace.

Default directories:

- Linux: `/dev/shm/ee/pack-l2/<workspace_id>/` when `/dev/shm` exists;
  otherwise the operating system's temporary directory.
- macOS: the operating system's temporary directory, normally selected by
  `TMPDIR`, followed by `ee/pack-l2/<workspace_id>/`.
- Explicit root override: `EE_L2_PACK_CACHE_DIR`; the workspace component is
  appended to that root. Non-Unix platforms currently bypass pack-cache reuse.

Writers create the workspace cache directory with mode `0700` on Unix. A missing
directory is a normal read-only cache miss; the reader does not create it. If an
attempted cache read or write fails, pack assembly remains available and reports
`l2_pack_cache_unavailable` with low severity. The `ee context` compatibility
alias does not inspect this directory because it bypasses L2.

## Canonical Key

For eligible requests, the L2 key is a BLAKE3 hash of the inputs that can affect
emitted pack content. Inputs without a supported freshness identity cause a
bypass rather than an incomplete key.
The canonical key schema is `ee.pack.l2_cache_key.v7`. It moved from v6 when
the pack hash moved to input schema `ee.pack.hash_input.v2` (ADR 0087), so an
entry cached under the old hash misses instead of replaying it. Addressed store identity,
logical generations, and the emitted embedding backend are separate inputs.
At minimum, the canonical input set includes:

- workspace ID
- addressed database filesystem identity
- database generation
- index generation
- graph generation when graph-derived fields can affect output
- active embedding backend (`neural_local` or `hash_fallback`)
- redaction level
- context profile
- resource profile
- max token budget
- candidate pool
- originally requested token/candidate limits and effective relevance floor
- memory scope
- strict-scope mode
- request query text
- explicit validity reference time and filters
- context feature flag set hash
- profile or personalization generation when per-agent profile bias applies

Same canonical input set means same key. Any changed input that can change
semantic pack JSON must change the key. Do not serve stale data and rely on downstream consumers
to notice.

Because model readiness may change while a pending request initializes the
local model, lookup and storage keys are intentionally distinct: lookup uses
the backend observed before assembly, while storage recomputes the key from
the backend that actually produced the response. Cached response payloads are
rejected unless their `data.embed_backend` exactly matches the key input.

## Cached Value

The cached response body is the successful producer's `ee pack --json` result.
Selected and omitted memories, provenance, scores, hashes, warnings, and other
semantic fields must match a fresh read-only assembly for the same eligible
request. This is not a promise of byte-identical fresh responses: the registered
diagnostic `data.pack.slo.actuals.elapsedMs` measures the assembly that produced
the body; `elapsedStatus` and aggregate `status` reflect that measurement. A hit
retains all three producer diagnostics. `resourceStatus`, pack hashes, and
content degradations remain deterministic. Current lookup timings and
hit status are available through `--read-only --explain-performance` and tracing.

The physical file uses the `ee.pack.l2_cache.entry.v2` storage wrapper. Its
decompressed payload is an `ee.pack.l2_context_response.v3` object containing
the response body in `responseJson`, a search-advisory snapshot, and source-mode
metadata used to reject unsafe fallback replays. Storage compression metadata
does not enter ordinary pack JSON.

The current entry wrapper contains its own compressed length, uncompressed
length, and BLAKE3 integrity hash. It does not publish the separate compression
manifest sidecar proposed in `docs/pack-compression.md`; that integration remains
unfinished. Compression representation must not change the canonical key or
the decompressed response.

## Read Path

1. Validate the request, establish the database snapshot, and check eligibility.
2. Compute the key and bounded index fingerprint when eligible.
3. For a read-only request, inspect L2 without modifying entries and recheck
   freshness before accepting a hit.
4. Assemble normally on a miss or bypass. Read-only assembly does not persist a
   pack, append pack audit rows, or repair a stale queued index.
5. For a persisted request, always assemble and perform persistence and audit
   work. Publish an eligible successful response only after those operations
   and the source-generation and index rechecks.

There is no full-response L1 lookup in front of this CLI L2 path. L2 inspection
does not acquire a cache lock. Writers publish a synced temporary file by atomic
rename, then sync the directory; readers reject incomplete or corrupt content.

## Write And Eviction

Cache writes are best-effort. A failed cache write must not fail the command or
undo a successfully persisted pack. The entry-size limit is 1 MiB; oversized
entries are skipped, and reads enforce bounded decompression.

Eviction is lazy and write-triggered. The default maximum size is 256 MiB per
workspace unless `EE_L2_PACK_CACHE_BYTES` overrides it. When the cap is
exceeded, eviction removes expired entries first, then entries with the oldest
modification time, with deterministic tie-breaking. Read-only pack hits do not
refresh that timestamp, so this path approximates publication age rather than
read popularity. Successful writes also prune older representations of the same
key. The default expiry age is 30 days from publication.

Eviction runs only on the writer path. Directory-level failures propagate as
cache-unavailable degradation; individual removal failures are counted as
skipped and may leave the cache over its configured cap. Per-file failure
degradation is not currently guaranteed.

## Failure Modes

Expected degraded codes:

- `l2_pack_cache_unavailable`: an attempted cache operation or required key-state
  read failed, including an unreadable or unwritable cache directory.
- `l2_pack_cache_corruption`: an entry exists but is not valid JSON or does not
  match the expected response shape, key, integrity hash, or compression data.

Both are response-time degradations. They should be low severity because normal
pack assembly remains available.

A missing or expired entry is an ordinary miss. Explicit disablement and
conservative eligibility bypasses do not emit either code merely because L2 was
not used. On corruption, a read-only request preserves the rejected entry and
assembles fresh output with the typed warning. A later successful persisted
producer can publish a valid replacement and prune the old representation;
read-only requests never perform this repair.

## Privacy

The L2 cache stores final emitted JSON, so it inherits the caller's redaction
level. Redaction level must be part of the canonical key. A redacted request and
an unredacted request must never share the same cache entry.

The cache directory mode must be `0700` to keep host-local agent artifacts out
of other users' accounts. Support bundles should report cache health and size,
not cached pack bodies.

## Configuration Keys

The configuration model accepts this shape:

```toml
[cache.pack_l2]
enabled = true
directory = ""
max_bytes = 268435456
max_age_days = 30
```

Currently, the presence of `.ee/config.toml` conservatively bypasses the CLI
cache before these settings are applied, even if `enabled = true`. Supporting
configured-workspace hits requires a complete configuration freshness identity;
the configuration model alone does not establish that support. Environment
overrides below apply to otherwise eligible requests without that file.

Environment variables:

- `EE_L2_PACK_CACHE_DISABLE`: disables L2 lookup and writes when `true`.
- `EE_L2_PACK_CACHE_DIR`: overrides the root cache directory.
- `EE_L2_PACK_CACHE_BYTES`: overrides the per-workspace byte cap.

All `EE_*` variables are registered in `src/config/env_registry.rs`.

## Tracing

Set `RUST_LOG=ee::pack_l2=debug` to inspect events, and `EE_LOG_FORMAT=json` for
structured stderr. Actual events use the `ee::pack_l2` target and an `event`
field:

- `pack_l2_cache_bypassed`, `pack_l2_cache_hit_bypassed`,
  `pack_l2_cache_hit_ignored`, and `pack_l2_cache_write_skipped` explain bypasses
  through `reason`.
- `pack_l2_cache_hit` includes the hashed `key`, local `path`, byte counts,
  compression metadata, and original storage time.
- `pack_l2_cache_miss` includes `key`, `reason`, and `fallback_reason`.
- `pack_l2_cache_write` includes `key`, `path`, `outcome`, byte and compression
  counts, `evicted`, and `bytes_removed`. Check `outcome`; an oversized response
  reports `skipped_too_large` rather than a stored entry.
- `pack_l2_index_fingerprint` includes `bytes_read`, `entry_count`, and
  `elapsed_us`. Sum repeated fingerprint events when measuring per-request cost.

These fields vary by event. The original proposed uniform request ID, phase,
degraded-code list, key-prefix, and aggregate cache-size fields are not all
implemented. The current key is a hash, but trace paths still reveal local
filesystem locations; ordinary redacted pack output has a different contract.

## Validation Checklist

- Same canonical inputs produce the same key across processes.
- Different redaction levels produce different keys.
- Different DB/index/graph generations produce different keys.
- A persisted producer creates a real entry while preserving pack and audit
  side effects; a matching read-only request produces an actual L2 hit.
- Hit and fresh read-only JSON match except the separately validated unsigned
  `data.pack.slo.actuals.elapsedMs`, `data.pack.slo.elapsedStatus`, and
  `data.pack.slo.status` diagnostics. Before normalizing, verify each elapsed
  classification against the published thresholds and each aggregate status
  against the resource and elapsed statuses. Selection, provenance, budget,
  hash, resourceStatus, and degradation fields remain asserted.
- Read-only hits and misses preserve database counts and source generation,
  index bytes, and cache file names, bytes, and modification times.
- Same-length index edits with restored modification times invalidate reuse.
- Corrupted entry falls through to fresh assembly and records degradation.
- The rejected corrupt entry remains unchanged during a read-only call.
- Unwritable cache directory records degradation and does not fail context.
- Cache directory is created with mode `0700`.
- Eviction reaches the configured per-workspace size cap when removals succeed.

The family integration scenario in `tests/e2e_core_workflow.rs` and inline cache
tests exercise these behaviors. Performance qualification must measure actual
hits against fresh read-only requests for the same indexed workload and report
fingerprint read cost alongside latency.

The original swarm goal of four simultaneous cold requests producing one
assembly and three hits is not implemented or qualified by these tests. There
is no cross-process single-flight assembly protocol; persisted calls always
perform their own work, and read-only misses do not populate L2. Broader backend,
configured-workspace, external-evidence, large-index, and non-Unix cache support
also remains outside the current eligible path.
