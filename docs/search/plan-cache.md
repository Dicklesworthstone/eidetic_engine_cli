# EQL query plan cache

> **Bead:** [`bd-2mey5`](https://github.com/Dicklesworthstone/eidetic_engine_cli) — idea-wizard MagentaPanther 2026-05-18.
> **Status (2026-05-19):** cache module shipped; `run_search_inner` integration and `ee diag plan-cache --json` surface tracked as follow-up work below.

## Why this exists

`ee context` and `ee search` re-parse the same EQL request thousands of times
per minute under swarm load. The dominant per-call cost is **bind +
index-selection** — not the BM25/vector retrieval itself — so the same agent
hammering the same prompt re-pays the same fixed cost on every shot.

Today the path looks like:

```text
EQL JSON request
   │
   ▼ parse_eql_query                                 (~10 μs)
   │
   ▼ bind + index choice + join strategy             (~40 μs, dominant)
   │
   ▼ search_sync → BM25 + vector + rerank            (varies)
```

The plan cache memoizes the resolved plan once and reuses it on every
subsequent call with the same request, the same index manifest, and the same
search config.

## Distinguishability versus neighboring caches

| Cache | What it caches | Invalidation | Tracked by |
|---|---|---|---|
| **L2 pack cache** | Final pack **results** | `(query, workspace, manifest)` change | `bd-ndzfg` |
| **Single-flight** | In-flight duplicate **calls** | Resolution / cancellation | `bd-gni47` |
| **Plan cache** (this doc) | Compiled **plan tree** between parse and execute | `(eql_hash, index_manifest_version, search_config_hash)` | `bd-2mey5` |
| **PPR prefetch cache** | Personalized PageRank score vectors | Snapshot generation | `bd-ov09.5` |

A miss in the L2 pack cache (for example, because a memory write advanced the
DB generation) still pays parse + bind + index-choice cost. The plan cache
eliminates that cost separately. Conversely, when the L2 pack cache hits, the
plan cache is not consulted.

## Cache key

```rust
pub struct PlanCacheKey {
    pub eql_hash: u64,
    pub index_manifest_version: u64,
    pub search_config_hash: u64,
}
```

All three components are 64-bit truncated `blake3` content hashes, computed
through domain-separated helpers in `src/search/plan_cache.rs`:

- `compute_eql_hash(canonical_request_bytes)` hashes the canonical EQL request
  payload (e.g. the serialized JSON document).
- `compute_search_config_hash(canonical_config_bytes)` hashes the resolved
  `SearchScoringConfig` (and any other config inputs that affect plan choice).
- `index_manifest_version` comes from the live `INDEX_MANIFEST_SCHEMA_V1`
  generation counter; bumps invalidate every prior plan entry automatically
  via key non-equality.

The hashes are domain-separated. `compute_eql_hash` and
`compute_search_config_hash` must produce different outputs for the same
input bytes; a regression test in `src/search/plan_cache.rs` pins this.

## Payload

```rust
pub struct CompiledPlan {
    pub parsed_query: EqlQuery,
    pub bound_index: Option<String>,
    pub join_strategy: Option<String>,
}
```

The current slice persists the parsed `EqlQuery`. The `bound_index` and
`join_strategy` fields stay `Option<None>` until the follow-up integration
bead extracts the compile step out of `run_search_inner`. The cache shape is
forward-compatible: adding the resolved index/join values does not change the
public types beyond filling those `Option` fields.

The plan-tree hash recomputes the canonical hash from the stored fields on
every `get`. A corrupted entry (e.g. invariant violation, manual surgery, or a
schema-tag mismatch from a future version) is treated as a miss; the entry
is dropped, `invalidations` is incremented, and the caller re-pays the
parse + bind cost.

## Eviction policy

- Bounded LRU with a deterministic tiebreaker by sorted key order.
- Capacity is set by `EE_QUERY_PLAN_CACHE_ENTRIES` (default `1024`).
- Capacity is silently clamped to `MAX_PLAN_CACHE_ENTRIES` (≈1M) so a
  misconfigured value cannot blow up memory.
- Capacity `0` disables caching entirely. `insert` succeeds (so callers can
  still surface the freshly computed plan-tree hash for tracing), but the
  entry is dropped immediately and every subsequent `get` reports a miss.
- `invalidate_other_generations(manifest, config)` drops every entry whose
  key does not match the supplied manifest and search-config hash. Useful for
  cooperative invalidation when a manifest publish lands without changing
  any EQL request itself.

## Observability

`PlanCache::stats()` exposes monotonic counters: `hits`, `misses`,
`inserts`, `evictions`, `invalidations`, plus the current cache size and
capacity. The follow-up `ee diag plan-cache --json` surface will expose these
through the standard CLI envelope and will include the top-N hot key
projections.

The bead tracing contract is `surface=plan_cache` with required fields
`workspace_id`, `eql_hash`, `manifest_version`,
`decision (hit|miss|invalidated)`, `elapsed_ms`, and `degraded_codes`. The
degraded vocabulary is:

| Code | Meaning |
|---|---|
| `plan_cache_disabled` | Capacity 0 or feature-flagged off. |
| `plan_cache_full` | Insert evicted at least one entry. |

These codes will be wired through the standard degraded aggregation pipeline
once `run_search_inner` consults the cache.

## Determinism

- The cache stores entries in a `BTreeMap`. `cached_keys()` iterates in
  sorted order so `ee diag plan-cache --json` and any future replay tooling
  is byte-stable.
- Tied LRU sequences break by sorted key so two caches that have observed
  the same insert/get history pick the same victim.
- The plan-tree hash domain tag (`ee.search.plan_cache.tree.v1`) bumps
  alongside any incompatible schema change so historical entries fail
  self-verification instead of returning stale data.

## Follow-up before `bd-2mey5` closes

The current slice keeps the cache reachable as a library type. The
remaining acceptance items live in follow-up beads:

- **Integration**: hook `src/core/search.rs:run_search_inner` so the
  call site at the `search_sync` invocation consults the plan cache and only
  re-runs parse + bind + index-choice on a miss. The current signature of
  `search_sync` takes `query: &str`; the integration slice will introduce a
  small "compile" step that returns a `CompiledPlan` and threads it through.
- **`ee diag plan-cache --json`**: expose `PlanCacheStats` and the top
  cached keys through the standard envelope.
- **Tracing**: emit the `surface=plan_cache` decision events on every
  `get`/`insert`.
- **Performance**: pin p50 EQL plan resolution `<2µs` on cache hit and
  no-regression on cache miss (bead acceptance #4 and #5).
- **End-to-end test** (`tests/plan_cache_e2e.rs`): drive `ee search` twice
  with the same query and assert the second call hits the plan cache.

Each follow-up will land its own bead under the `bd-2mey5` parent. This file
will gain a "Status" badge each time one of those acceptance items lands.
