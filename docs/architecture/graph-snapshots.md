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
- Dry-run prune reports stay read-only and do not write advisory-lock rows;
  durable prune acquires the prune lock and the matching refresh lock before
  listing or deleting candidates.

The refresh implementation uses these per-type `graph_snapshot` lock IDs, so
different graph families can refresh independently while same-type writers stay
serialized.

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

## Worked Refresh Example

A memory-link write can mark graph context stale until a refresh produces a new
valid snapshot. An agent should treat the stale signal as a graph-explanation
gap, not as a storage failure.

```bash
ee remember "RCH verification must stay remote-only." --workspace . --json
ee memory link mem_a mem_b --relation supports --workspace . --json
ee context "verify release" --workspace . --explain --json
```

Expected behavior:

```json
{
  "schema": "ee.response.v2",
  "success": true,
  "data": {
    "pack": {
      "packDna": {
        "schema": "ee.context.pack_dna.v1",
        "degraded": [
          {
            "code": "graph_snapshot_stale",
            "repair": "ee graph snapshot refresh --workspace . --json"
          }
        ]
      }
    }
  },
  "degraded": []
}
```

Agent interpretation: use the context pack, but do not cite Pack DNA as complete
until the graph snapshot is refreshed.

## Worked Prune Example

Snapshot pruning is safe only for archived derived rows. Agents should start
with dry-run mode and inspect counts before allowing a durable maintenance run:

```bash
ee maintenance graph-snapshot-prune --workspace . --dry-run --json
```

Expected dry-run shape:

```json
{
  "schema": "ee.graph.snapshot_prune.v1",
  "graphType": "memory_links",
  "dryRun": true,
  "candidateCount": 12,
  "prunedCount": 0,
  "candidateBytes": 4096,
  "prunedBytes": 0
}
```

Agent interpretation: a dry-run response is evidence for a maintenance plan,
not permission to delete source data. The prune surface never deletes memories,
links, evidence, revisions, packs, or source databases.

## Implementation Checklist

- Add per-graph-type advisory lock IDs instead of a workspace-wide graph
  snapshot lock. (Implemented for refresh writes and prune jobs.)
- Archive older valid snapshots inside the same transaction that inserts a new
  valid snapshot.
- Add repository helpers for listing and pruning archived snapshots by
  workspace, graph type, status, and cutoff timestamp. (Implemented for
  archived candidate listing and prune execution.)
- Add `GraphSnapshotPrune` to the maintenance job taxonomy. (Taxonomy entry and
  DB-backed runner surface are implemented for `memory_links`.)
- Add the `ee maintenance graph-snapshot-prune` command or an equivalent
  subcommand that delegates to the same job handler. (Dedicated CLI route is
  implemented and delegates to the `GraphSnapshotPrune` steward job.)
- Add `ee.graph.snapshot_prune.v1` schema documentation if the prune command
  emits its own payload. (Schema artifact added for the planned payload.)
- Add a focused unit test for per-type lock independence and prune/refresh
  conflict behavior.
- Add a focused unit test that a new valid snapshot archives only older valid
  snapshots of the same graph type.
- Add a focused unit test that prune removes only archived rows older than the
  retention cutoff.
- Add the 5k-memory storage-footprint contract test. (Implemented as a
  DB-level compact metrics payload contract for the five operational families.)

## Implementation Status

This page is the contract for `bd-bife.7`, but the bead is not complete until
the runtime and test evidence below exists. Keep this table in sync with tracker
comments so agents can distinguish documented policy from implemented behavior.

| Area | Current status | Remaining proof |
| --- | --- | --- |
| Lifecycle policy document | Documented here, including family list, status transitions, 7-day retention, locks, budgets, refresh cadence, and JSON shape. | Keep this page updated when the runtime surface changes. |
| Same-type archival | Implemented for persisted graph snapshot writes: a new valid snapshot archives prior valid snapshots for the same workspace and graph type. | Clean remote Cargo gate after active graph-wrapper compile blockers clear. |
| Per-type refresh locks | Implemented for refresh writes with graph-type scoped `graph_snapshot` advisory lock IDs. | Keep the focused lock-scope test green under the full remote suite. |
| Prune repository helpers | Implemented for archived candidate listing and prune execution in `DbConnection`. | Keep the focused archived-row safety test green under the remote suite. |
| Prune response schema | Documented as `docs/schemas/ee.graph.snapshot_prune.v1.json`; exported through the public schema registry with drift coverage. | Keep the schema aligned as graph-family and lock options expand. |
| Prune job taxonomy | Implemented as `graph_snapshot_prune` in the maintenance job taxonomy with DB-backed candidate listing and bounded archived-row pruning for `memory_links`. | Extend options to every operational graph family when those snapshot producers are wired. |
| Prune CLI/job surface | `ee maintenance graph-snapshot-prune --dry-run --json` delegates to the `GraphSnapshotPrune` steward job and emits `ee.graph.snapshot_prune.v1` details inside the maintenance response. Dry-runs report `lock.acquired=false`; durable runs include a concrete holder. | Add per-graph-type CLI options once non-`memory_links` families are live. |
| Prune locking | Implemented for durable prune jobs with a per-type `graph_snapshot_prune` advisory lock plus the matching per-type `graph_snapshot` lock, so pruning conflicts with same-family refresh while different graph families remain independently lockable. | Keep the focused prune/refresh conflict test green under the remote suite. |
| Prune safety test | Implemented as a focused DB helper test for archived same-type rows older than cutoff; CLI route has focused parser/JSON surface coverage and the steward runner has DB-backed lock/release coverage. | Extend with per-family CLI options once the non-`memory_links` snapshot producers are wired. |
| Storage footprint contract | Implemented as a DB-level 5k-memory fixture contract that stores one compact metrics payload for each operational snapshot family and checks <= 50 MB per family / <= 250 MB total. | Replace the compact fixture with live producer output once all five family builders are wired. |
| RCH verification | Partial historical focused passes exist for lock behavior; broad verification is currently blocked by unrelated graph-wrapper compile drift. | RCH-only focused and relevant broader gates after compile blockers clear; no local Cargo fallback. |

## Non-Goals

- Snapshot pruning does not delete memories, links, evidence, revisions, packs,
  or source databases.
- Snapshot pruning does not vacuum SQLite by itself.
- Snapshot refresh does not make graph analytics a prerequisite for search,
  context, or why. Graph features must degrade while core retrieval keeps
  working.
- The first lifecycle gate does not require a daemon. The CLI path must work on
  demand.
