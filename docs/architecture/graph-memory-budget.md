# Graph Memory Budget

Graph snapshots are derived assets. The durable memory store remains the source
of truth, and graph refreshes must degrade before they can exhaust process
memory. The runtime budget for graph work is enforced by
`src/core/graph_memory_budget.rs` and wired into the snapshot builders and
algorithm wrappers.

The default policy is deliberately conservative:

| Limit | Default | Purpose |
| --- | ---: | --- |
| `graph.memory.snapshot_cap_mb` | `250` | Hard cap for one graph snapshot family. |
| `graph.memory.per_algorithm_cap_mb` | `100` | Hard cap for one graph algorithm working set. |
| `graph.memory.degraded_below_pct` | `80` | Advisory threshold for near-cap snapshots. |
| `graph.memory.growth_multiplier_basis_points` | `15000` | In-build tripwire, where `15000` means `1.5x` the pre-build estimate. |

The pre-build estimator is:

```text
estimated_bytes = 32 * node_count + 96 * edge_count
```

The arithmetic saturates instead of wrapping. Extreme counts therefore refuse
graph work instead of producing a small estimate that would bypass the cap.

## Admission Points

Snapshot builders call `check_snapshot_admission` before allocating a
FrankenNetworkX graph. If the estimate is over the snapshot cap, the builder
skips the refresh and emits `large_graph_uncached`. If the estimate is admitted
but at or above the advisory threshold, callers can emit
`snapshot_approaching_cap` so operators see the workspace is close to the hard
limit.

Builders also call `check_in_build_growth` while constructing snapshots. If
observed allocation grows past the configured multiplier over the original
estimate, the build aborts with `unexpected_growth` and rolls back the partial
derived snapshot.

Algorithm wrappers call `check_algorithm_admission` before allocating scratch
state. A request larger than the per-algorithm cap refuses with
`algorithm_memory_cap`. A request that would fit by itself but would push active
graph memory over the snapshot cap refuses with `memory_pressure`.

Every refusal payload includes the observed bytes, limit bytes, severity,
message, and repair text. Call sites lower that payload into the normal
`degraded[]` response shape and graph telemetry/audit surfaces.

## Configuration

The documented config keys live under `[graph.memory]`:

```toml
[graph.memory]
snapshot_cap_mb = 250
per_algorithm_cap_mb = 100
degraded_below_pct = 80
growth_multiplier_basis_points = 15000
```

The same settings can be overridden with registered environment variables:

| Environment variable | Config key |
| --- | --- |
| `EE_GRAPH_MEMORY_SNAPSHOT_CAP_MB` | `graph.memory.snapshot_cap_mb` |
| `EE_GRAPH_MEMORY_PER_ALGORITHM_CAP_MB` | `graph.memory.per_algorithm_cap_mb` |
| `EE_GRAPH_MEMORY_DEGRADED_BELOW_PCT` | `graph.memory.degraded_below_pct` |
| `EE_GRAPH_MEMORY_GROWTH_MULTIPLIER_BASIS_POINTS` | `graph.memory.growth_multiplier_basis_points` |

See `docs/configuration/graph.md` for tuning guidance and `docs/env_vars.md`
for the environment registry.

## Operator Behavior

When a graph budget refusal appears, retrieval should continue through the DB,
search index, or any already-admitted graph snapshot. The failure is scoped to
the oversized derived graph work; it is not a storage failure.

Common repairs are:

- Raise `graph.memory.snapshot_cap_mb` for hosts with enough RAM.
- Raise `graph.memory.per_algorithm_cap_mb` for intentional maintenance jobs.
- Lower graph feature scope, sample size, or workspace size when running on a
  constrained foreground agent host.
- Retry after active graph work releases memory when the refusal is
  `memory_pressure`.

The default repair text is intentionally concrete because these codes can
surface in agent-readable JSON responses.

## Verification Anchors

The budget layer is covered by focused tests rather than a broad workspace
smoke test:

- Inline unit tests in `src/core/graph_memory_budget.rs` cover estimator
  arithmetic, refusal-code ordering, the 100k-memory graceful-refusal fixture,
  and deterministic serialization.
- `src/graph/mod.rs` covers snapshot admission and telemetry lowering at graph
  builder call sites.
- `src/graph/algorithms.rs` covers per-algorithm and active-memory refusals.
- `tests/contracts/graph_config_behavior.rs` verifies that config changes alter
  admission decisions.
- `fuzz/fuzz_targets/graph_memory_budget.rs` exercises admission boundaries and
  refusal-code stability.

Those anchors are the expected maintenance points when changing estimator
constants, refusal codes, config precedence, or telemetry payloads.
