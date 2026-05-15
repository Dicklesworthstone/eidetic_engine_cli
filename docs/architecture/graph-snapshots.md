# Graph Snapshot Lifecycle

This document is the lifecycle contract for graph-derived snapshots. The source
of truth remains FrankenSQLite. Graph snapshots are derived assets that can be
rebuilt from memory, link, evidence, and revision rows.

## Current Anchors

- `graph_snapshots` was introduced in migration V015 and expanded by V045 for
  typed graph families.
- `GraphSnapshotType` currently supports:
  - `memory_links`
  - `session_graph`
  - `procedure_graph`
  - `evidence_graph`
  - `composite`
  - `causal_evidence`
  - `revision_dag`
  - `rule_provenance`
  - `contradiction_subgraph`
- `GraphSnapshotStatus` currently supports `valid`, `stale`, `invalid`, and
  `archived`.
- `refresh_graph_snapshot` currently persists `memory_links` centrality
  snapshots and uses the advisory-lock table before durable writes.
- `ee status --json` reports the `memory_links` snapshot as the
  `graph_snapshot_artifact` derived asset.
- `ee graph export` can export persisted graph snapshots by ID or latest
  snapshot type.

## Snapshot Families

The graph-accretion rollout uses five operational snapshot families:

| Family | Stored type | Refresh trigger | Primary consumers |
| --- | --- | --- | --- |
| Memory links | `memory_links` | Every memory link write and explicit refresh | status, context graph enrichment, graph export |
| Causal evidence | `causal_evidence` | Every causal evidence insert | causal why, causal bottlenecks, lab replay |
| Revision DAG | `revision_dag` | Every memory edit or `logical_id` change | impact analysis, dominance frontiers |
| Rule provenance | `rule_provenance` | Nightly maintenance and explicit refresh | load-bearing memory badges |
| Contradiction subgraph | `contradiction_subgraph` | Every `contradicts` memory-link insert | structural health and contradiction clusters |

The older `session_graph`, `procedure_graph`, `evidence_graph`, and `composite`
types remain valid schema values, but they are not part of the first
graph-accretion lifecycle gate unless a later bead gives them a concrete
builder and consumer.

## Status Transitions

Snapshot status is per `(workspace_id, graph_type)`.

1. A successful durable refresh inserts a new `valid` snapshot.
2. Prior `valid` snapshots for the same `(workspace_id, graph_type)` become
   `archived` in the same transaction as the new insert.
3. A refresh precondition failure does not archive the prior valid snapshot.
   It reports a degraded code and leaves the last usable snapshot in place.
4. A validator can mark a snapshot `invalid` when its metrics JSON is malformed,
   its content hash does not match, or its schema is unsupported.
5. A stale detector can mark a snapshot `stale` when the source generation has
   advanced but a replacement has not been persisted yet.
6. `archived` and `invalid` snapshots are never selected as the latest usable
   snapshot for runtime features.

## Eviction Policy

Archived snapshots are bounded derived data.

- Retention target: keep archived snapshots for 7 days.
- On-demand command: `ee maintenance graph-snapshot-prune --workspace . --json`.
- Maintenance job equivalent: `ee maintenance run --job graph_snapshot_prune --json`.
- Dry-run mode must report candidate counts and bytes without deleting rows.
- Durable mode removes only rows with `status = archived` and
  `created_at < now - 7 days`.
- Durable mode must never remove `valid`, `stale`, or `invalid` rows.
- The source rows that built the snapshot are never deleted by snapshot pruning.

The prune command is intentionally separate from `cache_pruning`. General cache
cleanup can call the same implementation later, but the graph snapshot policy
needs its own JSON surface and tests because it carries graph-specific safety
rules.

## Locking Policy

Refresh and prune use advisory locks. The lock scope is the graph type, not the
whole workspace.

Canonical lock IDs:

| Operation | Resource type | Resource ID |
| --- | --- | --- |
| Refresh `memory_links` | `graph_snapshot` | `<workspace_id>:memory_links` |
| Refresh `causal_evidence` | `graph_snapshot` | `<workspace_id>:causal_evidence` |
| Refresh `revision_dag` | `graph_snapshot` | `<workspace_id>:revision_dag` |
| Refresh `rule_provenance` | `graph_snapshot` | `<workspace_id>:rule_provenance` |
| Refresh `contradiction_subgraph` | `graph_snapshot` | `<workspace_id>:contradiction_subgraph` |
| Prune archived rows | `graph_snapshot_prune` | `<workspace_id>:<graph_type>` |

Rules:

- Two refreshes of the same graph type serialize.
- Refreshes of different graph types may run in parallel.
- Pruning a graph type conflicts with refresh for the same graph type.
- The TTL is 300 seconds.
- Lock acquisition failure must surface a typed degraded code, not a panic or
  silent no-op.

The current implementation uses a workspace-scoped advisory lock for
`memory_links`; the lifecycle implementation should narrow that to the typed
lock IDs above.

## Memory And Disk Budgets

The target in-memory budget is 50 MB per snapshot family and 250 MB total for
the five operational families.

Required behavior:

- Workspaces below 10k memories may cache graph objects during a foreground
  command when the budget estimate fits.
- Workspaces at or above 10k memories should prefer rebuild-each-time mode for
  large graph objects unless an explicit background cache exists.
- A large graph that cannot fit the budget must produce a degraded code such as
  `graph.large_graph` while preserving lexical and direct database behavior.
- Snapshot metrics JSON should store only the topology and computed fields
  needed by consumers. It must not duplicate source-of-truth row bodies.

The storage-footprint contract for this bead is:

- A deterministic 5k-memory fixture persists all five operational snapshot
  families.
- Total `graph_snapshots.metrics_json` bytes for the fixture stay under 250 MB.
- The test reports per-family byte counts so regressions are actionable.

## Refresh Cadence

Refresh cadence is event-triggered unless the family is explicitly background
only.

| Family | Foreground write behavior | Background behavior |
| --- | --- | --- |
| `memory_links` | Refresh after a memory-link write when graph features are enabled | Can be rebuilt by centrality refresh |
| `causal_evidence` | Refresh after causal evidence insert | Can be rebuilt by maintenance |
| `revision_dag` | Refresh after memory edit or `logical_id` change | Can be rebuilt by maintenance |
| `rule_provenance` | Mark stale on rule/source mutation | Nightly refresh plus explicit graph refresh |
| `contradiction_subgraph` | Refresh after `contradicts` link insert | Can be rebuilt by maintenance |

When a foreground refresh would exceed its budget, the command should mark the
snapshot family stale and emit a recoverable degraded entry that points to the
explicit refresh command.

## JSON Surface Requirements

Every command that mutates or inspects graph snapshots must follow the response
envelope contract:

- JSON goes to stdout.
- Human diagnostics go to stderr.
- `degraded[]` is populated only when the current response is affected.
- `repair` points to a concrete command.
- Stable fields are ordered deterministically.

Minimum fields for prune responses:

```json
{
  "schema": "ee.graph.snapshot_prune.v1",
  "command": "maintenance graph-snapshot-prune",
  "workspaceId": "wsp_...",
  "graphType": "memory_links",
  "dryRun": true,
  "retentionDays": 7,
  "candidateCount": 0,
  "prunedCount": 0,
  "candidateBytes": 0,
  "prunedBytes": 0,
  "oldestRetainedAt": null,
  "lock": {
    "resourceType": "graph_snapshot_prune",
    "resourceId": "wsp_...:memory_links"
  }
}
```

## Implementation Checklist

- Add per-graph-type advisory lock IDs instead of the current workspace-wide
  graph snapshot lock.
- Archive older valid snapshots inside the same transaction that inserts a new
  valid snapshot.
- Add repository helpers for listing and pruning archived snapshots by
  workspace, graph type, status, and cutoff timestamp.
- Add `GraphSnapshotPrune` to the maintenance job taxonomy.
- Add the `ee maintenance graph-snapshot-prune` command or an equivalent
  subcommand that delegates to the same job handler.
- Add `ee.graph.snapshot_prune.v1` schema documentation if the prune command
  emits its own payload.
- Add a focused unit test for per-type lock independence.
- Add a focused unit test that a new valid snapshot archives only older valid
  snapshots of the same graph type.
- Add a focused unit test that prune removes only archived rows older than the
  retention cutoff.
- Add the 5k-memory storage-footprint contract test.

## Non-Goals

- Snapshot pruning does not delete memories, links, evidence, revisions, packs,
  or source databases.
- Snapshot pruning does not vacuum SQLite by itself.
- Snapshot refresh does not make graph analytics a prerequisite for search,
  context, or why. Graph features must degrade while core retrieval keeps
  working.
- The first lifecycle gate does not require a daemon. The CLI path must work on
  demand.
