# EQL query plan cache

> **Bead:** [`bd-2mey5`](https://github.com/Dicklesworthstone/eidetic_engine_cli) — idea-wizard MagentaPanther 2026-05-18.
> **Status (2026-11-27):** the search path consults the cache (`lookup_or_insert_process_plan` in `src/core/search.rs`), and `ee diag plan-cache --json` reports it. The cache lives in process memory, so read [Observability](#observability) before relying on its counters.

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

`compiled_search_plan` in `src/core/search.rs` fills all three fields. The
parsed query's `q` is the lexical engine's rewrite of the request text.
`bound_index` is the source mode (`lexical_only`, `semantic_only` or
`hybrid`), and `join_strategy` is `lexical_only`, `semantic_two_tier` or
`hybrid_two_tier`.

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
capacity. `ee diag plan-cache --json` reports them in the standard envelope,
with the first 16 cached keys in sorted key order.

The cache is a process-wide static, so the counters belong to the process
that ran the searches. A long-running process such as `ee serve` keeps one
cache across the searches it runs, but no long-running surface exposes
`diag plan-cache`. A separate
`ee diag plan-cache` process therefore always reports an empty cache: size 0,
zero hits and zero misses. What it can tell you today is the capacity and
where that value came from (`process_env` or `registry_default`).

Each lookup emits a `debug` event on the tracing target
`ee::search::plan_cache` with `workspace_id` (the index directory),
`eql_hash`, `index_manifest_version`, `search_config_hash`, `plan_tree_hash`,
`cache_decision`, `capacity`, `evicted` (a count), `degraded_codes`,
`elapsed_ms` and `source_mode`. `degraded_codes` is always `[]`.

| Code | Where it is reported |
|---|---|
| `plan_cache_disabled` | `ee diag plan-cache`, when `EE_QUERY_PLAN_CACHE_ENTRIES` is 0. Search output does not report it. |
| `plan_cache_full` | Nowhere yet. An eviction shows only as the trace event's `evicted` count. |

## Determinism

- The cache stores entries in a `BTreeMap`. `cached_keys()` iterates in
  sorted order so `ee diag plan-cache --json` and any future replay tooling
  is byte-stable.
- Tied LRU sequences break by sorted key so two caches that have observed
  the same insert/get history pick the same victim.
- The plan-tree hash domain tag (`ee.search.plan_cache.tree.v1`) bumps
  alongside any incompatible schema change so historical entries fail
  self-verification instead of returning stale data.

## What is still open

`bd-2mey5` is closed, and the search integration, the `diag` surface and the
trace event have landed. Three things are not done:

- **Performance.** No bench pins plan resolution time on a hit or a miss, so
  the `<2µs` hit target in the bead's acceptance is unmeasured.
- **Live counters from the CLI.** `ee diag plan-cache` cannot read another
  process's cache (see [Observability](#observability)), so an end-to-end
  test that runs `ee search` twice and expects a hit is not possible as
  separate processes. `tests/plan_cache_unit.rs`
  (`process_cache_reuses_compiled_plan_and_diag_reports_live_counters`)
  checks hit counting inside one process instead, and
  `tests/diag_plan_cache_e2e.rs` checks only the envelope of a real `diag`
  run.
- **Degraded codes on search output.** Neither `plan_cache_disabled` nor
  `plan_cache_full` reaches `ee search` or `ee context` output.
