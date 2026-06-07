# Agent Onboarding: Graph Insights

This guide is for coding agents that need to use graph-derived `ee` surfaces
without reading implementation code. Treat the JSON schemas as the contract and
the commands below as inspection tools. These surfaces explain graph posture;
they do not replace `ee pack`, `ee search`, or `ee why`.

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

`availableSections` means the binary recognizes the section name, not that the
section has DB-backed evidence today. If `degradedSignals[]` contains
`insights_section_unavailable`, treat the listed sections as metadata-only
registered surfaces. They may return schema-valid empty `items[]`, but agents
should not use them as evidence until the degraded signal disappears.

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

If `degradedSignals[]` contains `insights_section_unavailable`, prefer sections
with non-empty evidence. The current metadata-only registered sections are
`comprehensiveRules`, `kCore`, `kTruss`, and `revisionFrontiers`.

## Section Workflow

Use section mode when you already know what decision you are making:

```bash
ee insights --section bridges --workspace . --json
ee insights --section contradictionClusters --workspace . --json
ee insights --section proximityHotspots --workspace . --json
ee insights --section knowledgeSkyline --workspace . --json
ee insights --section hubs --workspace . --json
ee insights --section authorities --workspace . --json
ee insights --section loadBearingMemories --workspace . --json
```

Agent interpretation rules:

- `authorities`: prefer these memories when a task needs grounded claims and
  evidence that many navigation memories point toward.
- `hubs`: use these memories as orientation anchors when a task needs a map of
  related authoritative facts.
- `loadBearingMemories`: preserve or review these memories before curation,
  decay, or handoff because many procedural rules cite them.
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
ee pack "prepare release" --workspace . --explain --json
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

## HITS Profiles

Use HITS sections when a task depends on the direction of memory links:

```bash
ee insights --section authorities --workspace . --json \
  | jq '.data.sections[] | select(.name == "authorities") | .items[0]'
ee insights --section hubs --workspace . --json \
  | jq '.data.sections[] | select(.name == "hubs") | .items[0]'
```

Authority items expose `authorityScore`, `interpretation: "authority"`, and
`evidence.schema: "ee.graph.hits.v1"`. Hub items expose `hubScore`,
`interpretation: "hub"`, and the same HITS evidence schema. Both sections use
`hits_centrality_directed` over the memory-link graph.

When an authority or hub appears in a pack, follow with `ee why` to see the
per-memory HITS explanation:

```bash
ee why mem_authority --workspace . --json \
  | jq '.data.graph.hits'
```

Inspect `authorityScore`, `authorityRank`, `hubScore`, `hubRank`,
`dominantRole`, `profileInfluence`, and `rationale`. `dominantRole` set to
`"authority"` means the memory is more useful as grounded evidence;
`dominantRole` set to `"hub"` means it is more useful as a navigation anchor.

Context profiles consume the same scores. Use `grounding` when the pack should
favor authoritative memories, `orientation` when it should favor hub memories,
and `balanced` when neither direction should dominate:

```bash
ee pack "ground release evidence" --profile grounding --workspace . --json
ee pack "map release dependencies" --profile orientation --workspace . --json
ee pack "prepare release" --profile balanced --workspace . --json
```

## Load-Bearing Memories

Use load-bearing insights when a task may edit, tombstone, or consolidate
memories that procedural rules depend on:

```bash
ee insights --section loadBearingMemories --workspace . --json \
  | jq '.data.sections[] | select(.name == "loadBearingMemories") | .items[0]'
```

Items expose `loadBearingScore`, `citingRuleCount`, `interpretation`, and
`evidence.algorithm`. `interpretation` is `"load_bearing"` and
`evidence.algorithm` is `"bipartite_hits"`. The evidence is a
rule-to-source provenance projection: memory nodes score as authorities
when many rule nodes cite them.

Follow with `ee why` before changing a listed memory:

```bash
ee why mem_load_bearing --workspace . --json \
  | jq '.data.graph.loadBearing'
```

Inspect `isLoadBearing`, `loadBearingScore`, `authorityRank`,
`citingRuleCount`, `citingRules`, `evidence.projection`, and `rationale`.
`citingRules[]` should expose rule IDs and relations, not raw rule bodies, so
agents can decide whether to preserve the memory or review the dependent rules
without leaking source text into handoffs.

## Consumer Checklist

- Parse by `schema`, not by command name.
- Treat unknown section names as forward-compatible data.
- Treat unknown fields inside known schemas as a schema violation unless the
  schema version changes.
- Sort nothing yourself unless the schema says the array is unordered.
- Keep graph-derived output separate from provenance. Graph signals explain
  relationships; provenance still comes from the memory records and evidence
  links.
