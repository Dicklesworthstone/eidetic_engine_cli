# Agent UX Cancellation Contract

This document describes how agents should interpret cancellation, timeout, and
budget behavior for graph-accretion and agent-facing `ee` commands. It is a
consumer contract, not an implementation guide.

## Core Rule

Cancellation is a bounded outcome. A cancelled or timed-out graph operation
must either return a structured degraded signal or fail with a stable error
code. It must not emit partial machine JSON as if it were complete.

For machine-facing commands, a successful envelope with partial graph data must
explain the partial state through `degraded[]` or a surface-specific degraded
array such as `degradedSignals[]`.

## Agent Behavior

Agents should follow this order:

1. Check process exit code.
2. If stdout is JSON, parse the envelope and inspect `success`.
3. Inspect `degraded[]`, `degradedSignals[]`, or the surface-specific degraded
   field before using graph findings.
4. If the command timed out, retry only after narrowing the surface, lowering
   the limit, or selecting a cheaper section.

Do not retry the same expensive graph command in a tight loop. Prefer a section
or explain query that asks for fewer graph products.

## Budgeted Graph Commands

Graph-accretion commands use bounded wrappers for expensive work. Timeouts and
sampling decisions are part of the answer:

- `ee insights --json` is the broadest graph inspection surface.
- `ee insights --section <name> --json` narrows the budget to one section.
- `ee insights --explain <memory-id> --json` narrows the budget to one memory.
- `ee context --explain --json` may include Pack DNA, but ordinary context
  selection still remains useful if Pack DNA degrades.

Worked example:

```json
{
  "schema": "ee.insights.v1",
  "mode": "section",
  "selectedSection": "proximityHotspots",
  "sections": [],
  "degradedSignals": [
    {
      "code": "graph_algorithm_timeout",
      "severity": "medium",
      "message": "proximityHotspots exceeded the graph budget.",
      "repair": "Retry with a smaller limit or rebuild graph snapshots."
    }
  ]
}
```

Agent response: do not assume there are no hotspots. Retry with a smaller
section query or continue without proximity evidence.

## Graph Wrapper Implementation Pattern

All graph algorithm wrappers route through `src/graph/algorithms.rs` and pass an
Asupersync `&Cx` into `run_with_budget`. The wrapper checks `cx.checkpoint()`
before admission and then polls for cancellation every 10ms while awaiting the
blocking algorithm worker.

The cancellation model is intentionally soft for CPU-bound `fnx_algorithms`
calls. Asupersync can stop waiting on a blocking worker and suppress its result,
but Rust cannot safely kill an already-running CPU closure. Wrappers that call
other graph helpers should clone and pass the same `Cx` so nested work observes
the same cancellation request before it starts a second budgeted operation.

Use the explicit `*_with_cx` wrapper when a caller already owns a request
context. Synchronous convenience wrappers fall back to `Cx::current()` and then
`Cx::for_testing()` for unit tests.

## Context Pack Degradation

For `ee context --explain --json`, graph explanation failures must not poison
the base pack. A Pack DNA timeout means the graph explanation is incomplete; it
does not mean the selected pack items are invalid.

Worked example:

```json
{
  "schema": "ee.context.pack_dna.v1",
  "snapshotVersion": 42,
  "voronoiDominator": null,
  "communityOfMass": null,
  "egoSubgraph": {"nodes": [], "edges": []},
  "pprNeighbors": [],
  "degraded": [
    {
      "code": "graph_pack_dna_no_dominator",
      "severity": "low",
      "repair": "Rebuild graph snapshot or inspect a smaller pack."
    }
  ]
}
```

Agent response: use `pack.items[]` for the task, and mention that graph
composition evidence was unavailable or partial.

## Cancellation-Safe Output

A command that is interrupted before it can produce a valid JSON envelope should
write diagnostics to stderr and exit non-zero. A command that can produce a
valid partial envelope should keep stdout parseable and put all human-oriented
diagnostics on stderr.

Consumers may treat these fields as volatile and unsuitable for stable hashes:

- `generatedAt`
- `runDurationMs`
- `snapshotRefreshedAt`
- `witnessElapsedMs`
- `witnessRecordedAt`
- `algorithmStartedAt`

Semantic hashes should strip volatile fields before comparison. Ranking,
memory IDs, section names, and stable degraded codes are not volatile.

## Safe Retry Patterns

Use these narrower retries:

```bash
ee insights --section topMemories --workspace . --json
ee insights --section bridges --workspace . --json
ee context "prepare release" --workspace . --explain --max-tokens 2000 --json
ee proximity mem_a mem_b --workspace . --json
```

Avoid these patterns:

- Re-running full `ee insights --json` repeatedly after a timeout.
- Treating an empty graph section with degraded signals as a true empty result.
- Mixing stderr progress text into JSON parsing.
- Retrying with local Cargo or local heavy verification when the repo requires
  RCH for CPU-intensive work.
