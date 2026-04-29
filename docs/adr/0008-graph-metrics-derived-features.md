# ADR 0008: Graph Metrics Are Explainable Derived Features

Status: accepted
Date: 2026-04-29

## Context

Memory, evidence, decisions, sessions, artifacts, and rules form a graph.
Graph metrics can improve retrieval and explanations, but opaque graph boosts
could also dominate trustworthy evidence or make ranking hard to debug.

## Decision

Graph analytics are derived, explainable features built from durable records.
FrankenNetworkX provides graph projections and algorithms. `ee` does not use
petgraph or hand-maintained graph metrics for core behavior. If graph snapshots
are missing or stale, retrieval continues without graph boosts and reports the
degraded state.

## Consequences

Graph features can improve ranking, neighborhood inspection, and `why` output
without becoming a hidden source of truth. Graph snapshots are rebuildable and
versioned against database generations.

Graph-enhanced retrieval must remain explainable: output should say which graph
feature mattered, what it was derived from, and whether it was stale.

## Rejected Alternatives

- petgraph as the core graph layer.
- Hand-rolled PageRank, betweenness, or community detection.
- Persisting graph metrics as authoritative state.
- Failing retrieval when graph snapshots are unavailable.

## Verification

- Dependency audits fail on petgraph.
- Graph contract tests compare tiny known graph metrics and deterministic
  witness hashes.
- Degradation tests prove stale or unavailable graph snapshots remove boosts
  while preserving lexical/manual retrieval.

