# Agent Onboarding: Graph Insights

This guide is for coding agents that need to use graph-derived `ee` surfaces
without reading implementation code. Treat the JSON schemas as the contract and
the commands below as inspection tools. These surfaces explain graph posture;
they do not replace `ee context`, `ee search`, or `ee why`.

## First Probe

Start with the full insights bundle when you need a quick map of a workspace:

```bash
ee insights --workspace . --json
```

The response data uses `ee.insights.v1`. Read these fields first:

- `mode`: `full_bundle`, `section`, or `explain`.
- `availableSections`: the stable section names this binary knows.
- `sections[]`: one object per returned section, each with `name`,
  `summary`, `whyItMatters`, `items`, and `nextCommands`.
- `degradedSignals[]`: section-level graph problems. Do not ignore this array;
  it explains empty or missing sections.

Worked example:

```json
{
  "schema": "ee.insights.v1",
  "mode": "full_bundle",
  "availableSections": ["topMemories", "bridges", "proximityHotspots"],
  "sections": [
    {
      "name": "bridges",
      "summary": "Top articulation-point memories ranked by cluster-disconnection-magnitude.",
      "items": [
        {
          "rank": 1,
          "memoryId": "mem_release_policy",
          "articulationPoint": "mem_release_policy"
        }
      ],
      "nextCommands": ["ee insights --section bridges --workspace . --json"]
    }
  ],
  "degradedSignals": []
}
```

If `sections[]` is empty and `degradedSignals[]` contains
`graph.workspace_empty`, seed memories or use `ee remember` before treating the
graph as informative.

## Section Workflow

Use section mode when you already know what decision you are making:

```bash
ee insights --section bridges --workspace . --json
ee insights --section contradictionClusters --workspace . --json
ee insights --section proximityHotspots --workspace . --json
ee insights --section knowledgeSkyline --workspace . --json
```

Agent interpretation rules:

- `bridges`: preserve or review load-bearing memories before decay or
  tombstone work.
- `contradictionClusters`: curate the cluster before relying on any one memory
  as policy.
- `proximityHotspots`: pack, review, or edit tightly coupled memories together.
- `knowledgeSkyline`: inspect workspace-level risk before broad retrieval or
  release work.

Worked example:

```bash
ee insights --section proximityHotspots --workspace . --json \
  | jq '.data.sections[] | select(.name == "proximityHotspots") | .items[0]'
```

Expected use: take the two memory IDs from the hotspot item and follow with
`ee proximity` when you need the pairwise min-cut path and interpretation:

```bash
ee proximity mem_a mem_b --workspace . --json
```

## Context Pack DNA

Use Pack DNA when a context pack seems surprising:

```bash
ee context "prepare release" --workspace . --explain --json
```

The graph block uses `ee.context.pack_dna.v1` and may contain:

- `voronoiDominator`: the selected memory dominating the local evidence region.
- `communityOfMass`: the community carrying most of the pack's graph mass.
- `egoSubgraph`: the local node and edge neighborhood.
- `pprNeighbors`: Personalized PageRank neighbors that explain graph pull.

Worked example:

```json
{
  "schema": "ee.context.pack_dna.v1",
  "voronoiDominator": {
    "memoryId": "mem_release_policy",
    "reason": "selected item dominates the local evidence neighborhood"
  },
  "communityOfMass": {
    "communityId": "release-readiness",
    "mass": 0.72
  },
  "pprNeighbors": [
    {"memoryId": "mem_rch_remote_required", "score": 0.41, "rank": 1}
  ],
  "degraded": []
}
```

If `degraded[]` is non-empty, trust the ordinary pack items first and use the
graph explanation as partial evidence only.

## Why, Health, Skyline, And Proximity

Use the narrower surfaces when a task needs one specific graph question:

```bash
ee why mem_release_failure --workspace . --causal-explain --json
ee health --robot-insights --workspace . --json
ee status --skyline --workspace . --json
ee proximity mem_release_policy mem_rch_remote_required --workspace . --json
```

What to inspect:

- `ee.why.causal.v1`: `paths[]` and `minCut` show causal ancestry and
  bottlenecks.
- `ee.health.structural.v1`: `kTruss`, `contradictionClusters`, and `summary`
  identify structural support or incoherence.
- `ee.status.skyline.v1`: `skyline[]` and `summary` show portfolio-level memory
  posture.
- `ee.proximity.v1`: `minCut`, `interpretation`, and `treePath` show pairwise
  graph closeness.

Worked example:

```json
{
  "schema": "ee.proximity.v1",
  "memoryA": "mem_release_policy",
  "memoryB": "mem_rch_remote_required",
  "minCut": 0.31,
  "interpretation": "strong",
  "treePath": ["mem_release_policy", "mem_rch_remote_required"],
  "degraded": []
}
```

Use strong proximity as a packing and review hint, not as proof that the two
memories are true.

## Consumer Checklist

- Parse by `schema`, not by command name.
- Treat unknown section names as forward-compatible data.
- Treat unknown fields inside known schemas as a schema violation unless the
  schema version changes.
- Sort nothing yourself unless the schema says the array is unordered.
- Keep graph-derived output separate from provenance. Graph signals explain
  relationships; provenance still comes from the memory records and evidence
  links.
