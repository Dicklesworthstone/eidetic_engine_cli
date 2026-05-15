# ADR 0034: Multi-Graph Snapshot Framework

Status: proposed
Date: 2026-05-15
Bead: bd-rnfh.6

## Context

ADR 0008 makes graph analytics derived, explainable features rather than
authoritative state. The first graph implementation centered on
`memory_links`, but the graph-accretion work needs several projections whose
edge meanings and attributes are not interchangeable:

- `memory_links` stores general memory relationships, weights, confidence, and
  provenance.
- `causal_evidence` stores failure-to-cause direction, contribution scores,
  methods, evidence URIs, and computation time.
- `revision_dag` stores memory revision lineage and derived-from edges.
- `rule_provenance` is bipartite: rule nodes connect to source-memory nodes
  with role and support attributes.
- `contradiction_subgraph` is a filtered projection over contradictory memory
  links for structural-health diagnostics.

These projections share lifecycle machinery: snapshot versioning, content
hashes, stale/invalid/archived states, CGSE witnesses, and result caches. They
do not share a clean edge schema. Treating them as one combined graph would
make every consumer defend against unrelated attributes and would blur the
provenance of graph-derived scores.

## Decision

`ee` will use a multi-graph snapshot framework over typed projections backed by
`fnx_classes` graph types and FrankenNetworkX algorithms. The five operational
snapshot families are:

| Family | Stored `graph_type` | Primary role |
| --- | --- | --- |
| Memory links | `memory_links` | Context ranking, centrality, graph export |
| Causal evidence | `causal_evidence` | Causal why, causal bottlenecks, replay |
| Revision DAG | `revision_dag` | Revision impact and dominance-frontier analysis |
| Rule provenance | `rule_provenance` | Load-bearing rule/source-memory analysis |
| Contradiction subgraph | `contradiction_subgraph` | K-truss and contradiction-cluster health |

Each snapshot family has its own deterministic content hash. The graph type is
part of the hash domain, so identical JSON topology under two different graph
families does not produce the same semantic snapshot identity.

The source of truth remains FrankenSQLite through SQLModel/FrankenSQLite
repositories. `graph_snapshots`, `graph_algorithm_witnesses`, and
`graph_algorithm_results` are derived tables. They can be pruned, invalidated,
or rebuilt without changing memories, links, causal evidence, revisions, or
rules.

The older schema values `session_graph`, `procedure_graph`, `evidence_graph`,
and `composite` remain valid stored values for compatibility with existing
migrations, but they are not operational families for the first F1 gate unless
a later bead adds a builder and consumer contract.

## Refresh Cadence

Refresh cadence is per family, not workspace-global:

| Family | Foreground behavior | Background behavior |
| --- | --- | --- |
| `memory_links` | Refresh after memory-link writes when graph features are enabled | Centrality refresh can rebuild |
| `causal_evidence` | Refresh after causal evidence inserts | Maintenance can rebuild |
| `revision_dag` | Refresh after memory edits or `logical_id` changes | Maintenance can rebuild |
| `rule_provenance` | Mark stale after rule/source mutation | Nightly or explicit refresh |
| `contradiction_subgraph` | Refresh after `contradicts` link inserts | Maintenance can rebuild |

Refreshes use advisory locks scoped to `(workspace_id, graph_type)`. A
`causal_evidence` refresh must not block a `memory_links` refresh, but two
refreshes for the same graph family serialize.

## Consequences

Consumers can ask for the graph family that matches their semantics instead of
filtering a combined graph at runtime. This keeps edge attributes homogeneous
inside a projection and makes algorithm witnesses easier to interpret.

Snapshot hashes and cached results are stable because every cache key includes
the graph type, snapshot hash, algorithm name, and canonical parameters.
Deterministic ordering is required when builders read database rows so the same
database state yields byte-identical graph payloads and pack outputs.

The framework adds lifecycle responsibility. Every new graph family needs:

- A typed builder from authoritative tables.
- A schema version for persisted metrics JSON.
- A content-hash function with the graph type in the hash domain.
- Stale/refresh behavior and repair text.
- Degraded behavior when the snapshot is missing, stale, too large, or invalid.
- Contract tests proving rebuildability and deterministic output.

Graph snapshots remain optional derived assets. Search, context, and why
surfaces must keep working in degraded mode when a graph family is unavailable.

## Rejected Alternatives

- **Single combined graph with typed edges**: This would mix heterogeneous
  attributes such as causal contribution scores, revision transition kinds, and
  bipartite rule roles. It also makes deterministic witnesses harder to read
  because algorithm inputs contain unrelated edge families.
- **One subgraph per relation type inside `memory_links`**: This would split
  the system into many low-value refresh families while still failing to cover
  projections whose source is not `memory_links`, such as causal evidence and
  revision lineage.
- **Build every projection on demand per query**: This violates ADR 0008's
  derived-but-cacheable model and turns every insights or context call into an
  O(N) graph-construction path.
- **Persist graph metrics as authoritative rows**: This would make graph
  outputs harder to repair after algorithm or schema changes. Metrics must be
  rebuildable from source tables and snapshots.

## Verification

The framework is not complete until these checks exist and pass:

- `tests/contracts/multi_graph_snapshots.rs` seeds all five operational
  families and verifies their stored `graph_type`, schema version, content hash,
  node count, edge count, and status.
- Builder unit tests cover empty input, deterministic row ordering, attribute
  preservation, and graph-family-specific invariants.
- Content-hash tests prove the graph type is part of the hash domain; the same
  metrics JSON under `memory_links` and `causal_evidence` must hash
  differently.
- Lifecycle tests prove a new valid snapshot archives only older valid
  snapshots of the same `(workspace_id, graph_type)`.
- Degraded-mode tests prove missing, stale, invalid, or oversized snapshots
  remove graph boosts while preserving core CLI behavior.
- `docs/architecture/graph-snapshots.md` remains the operational lifecycle
  contract for refresh cadence, locks, eviction, and memory/disk budgets.
