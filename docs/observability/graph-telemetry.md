# Graph Telemetry (bd-bife.22)

`ee` exposes graph-operation visibility through well-known `tracing`
event targets. Operators pipe the events into any subscriber
(`tracing-subscriber::fmt`, `tracing-opentelemetry`, custom JSON
formatters, …); we do **not** ship a Prometheus client or any other
exporter. The decision is documented in `bd-bife.22`: tracing
events are zero-friction in this codebase, level filters honor
`EE_TRACE` / `RUST_LOG`, and any downstream observability stack
that speaks `tracing` consumes them without further integration.

The canonical event names live as Rust constants in
`src/core/graph_telemetry.rs`; this page restates them in the
machine-and-human-readable form an operator query language can
match against.

## Field convention

All graph-telemetry events use the snake_case field names from
[`docs/observability/tracing_field_convention.md`](tracing_field_convention.md).
The fields below are documented per-event; nothing else is emitted
by the helpers (so a query like
`target = "ee.graph.algorithm.compute" AND cache_hit = true` is
sufficient).

## Event taxonomy

| Target | Level | Emitted when | Required fields |
| --- | --- | --- | --- |
| `ee.graph.snapshot.refresh`   | `info`  | A persisted graph snapshot has just been rebuilt. | `graph_type`, `snapshot_version`, `build_ms`, `node_count`, `edge_count`, `lock_wait_ms` |
| `ee.graph.algorithm.compute`  | `debug` | One algorithm invocation completed (fresh compute or cache-served). | `algorithm`, `snapshot_id`, `params_hash`, `elapsed_ms`, `cache_hit`, `sampling_used` |
| `ee.graph.algorithm.timeout`  | `warn`  | An algorithm invocation hit its `run_with_budget` ceiling. | `algorithm`, `snapshot_id`, `budget_ms`, `elapsed_ms` |
| `ee.graph.algorithm.cancelled`| `info`  | An algorithm invocation was cancelled via the `Cx` cancellation path. | `algorithm`, `elapsed_ms` |
| `ee.graph.cache.hit`          | `trace` | Algorithm-result cache served a request without recomputation. | `algorithm`, `params_hash` |
| `ee.graph.cache.miss`         | `trace` | Algorithm-result cache missed and recomputation followed. | `algorithm`, `params_hash` |
| `ee.graph.cache.evict`        | `debug` | One or more cache rows were evicted by the maintenance path. | `reason`, `count` |

### `cache.evict` reason values

The `reason` field on `ee.graph.cache.evict` is one of (mirrors
`crate::core::graph_telemetry::CacheEvictReason::as_str`):

- `ttl_expired` — row aged past the configured retention TTL.
- `snapshot_archived` — the snapshot the row was tied to has been archived.
- `operator_request` — operator invoked the maintenance command explicitly.

New reason values must be added to both the Rust enum and this list
so query languages do not see undocumented strings.

## Recipe: hit rate by algorithm

With a `tracing-subscriber::fmt` configured for JSON output, an
operator can compute the algorithm-result cache hit rate over a
window as:

```text
target = "ee.graph.algorithm.compute"
group_by algorithm
hit_rate = count_if(cache_hit = true) / count(*)
```

`ee.graph.algorithm.compute` was deliberately chosen as the single
event for both fresh and cache-served outcomes; `ee.graph.cache.hit`
and `ee.graph.cache.miss` exist as finer-grained TRACE-level
breadcrumbs for deep debugging but should not be required for
operator-facing dashboards.

## Recipe: timeout pressure by algorithm

Timeouts surface at WARN so they show up at default filters without
extra configuration:

```text
target = "ee.graph.algorithm.timeout"
group_by algorithm
count over last 1h
```

When the count is non-zero an operator should compare `elapsed_ms`
against `budget_ms`; values clustering near the budget mean the
budget itself is the throttle, while values far above the budget
indicate the timeout is firing late and the snapshot may be the
underlying problem.

## Recipe: snapshot churn

`ee.graph.snapshot.refresh` fires every time a snapshot rebuild
completes. `build_ms` plus `lock_wait_ms` together describe the
total operator-visible refresh cost; high `lock_wait_ms` indicates
contention with concurrent agents rather than algorithm work.

```text
target = "ee.graph.snapshot.refresh"
plot p50(build_ms) over time
plot p99(lock_wait_ms) over time
```

## Wiring status

This page documents the emission **contract** and the current
production call-site wiring:

- F2 algorithm wrappers emit `ee.graph.algorithm.compute`,
  `ee.graph.algorithm.timeout`, and `ee.graph.algorithm.cancelled`.
- The algorithm-result cache emits `ee.graph.cache.hit`,
  `ee.graph.cache.miss`, and TTL-driven `ee.graph.cache.evict`.
- Snapshot persistence emits `ee.graph.snapshot.refresh`, and emits
  `ee.graph.cache.evict` with `reason = "snapshot_archived"` when a
  new graph snapshot evicts stale `graph_algorithm_results` rows tied
  to older snapshots.

The `operator_request` eviction reason is reserved in the public enum
and docs, but no production operator-request cache-eviction command is
currently wired to emit it. That call site remains bd-2inbn follow-up
work.

## Versioning

The event-target strings are part of `ee`'s public observability
contract; renames are major-version changes. Field additions are
allowed without a version bump as long as existing field names and
types remain. Field removals are major-version changes.
