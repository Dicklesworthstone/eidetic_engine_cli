# Agent UX Schema Evolution Policy

Graph-accretion schemas are stable public contracts once advertised by
`ee::core::supported_schemas()`.

Agents should parse the `schema` field before reading any graph-derived block.
Command names are routing hints; schema IDs are the durable compatibility
boundary.

## Version Rules

- A `.v1` schema is frozen once shipped.
- New fields are additive only and must be optional unless the schema version is
  bumped.
- Renames, removals, type changes, or required-field additions require a new
  `.v2` schema.
- During a migration window, commands that emit both old and new schema shapes
  must include a `degraded` entry with code `schema_evolution` and a repair
  action that points consumers to the migration document.
- Schema IDs in `supported_schemas()` must have a matching
  `docs/schemas/*.json` document.
- Any schema exposed through a graph-accretion command must also have a stable
  snapshot under `tests/snapshots/` before the owning bead can close.

## Consumer Rules

- Reject a known schema version if a required field is missing.
- Ignore an unknown schema ID unless the task explicitly requires that surface.
- Treat unknown optional fields on a known schema as unavailable unless a newer
  migration document says how to consume them.
- Do not infer schema version from command flags. `ee context --explain` can
  contain multiple nested schemas.
- Do not store or compare volatile fields as semantic evidence. Use the
  volatile-field registry before hashing graph output.

## Graph-Accretion Scope

The first graph-accretion schema set is:

- `ee.insights.v1`
- `ee.context.pack_dna.v1`
- `ee.why.causal.v1`
- `ee.health.structural.v1`
- `ee.status.skyline.v1`
- `ee.memory.impact_analysis.v1`
- `ee.proximity.v1`
- `ee.why.v1`
- `ee.context.v1`

`ee.why.v1` and `ee.context.v1` are augmentation contracts. They govern
additive graph-derived blocks on existing command output; they do not permit
renaming or removing current fields.

## Worked Examples

Nested context output:

```json
{
  "schema": "ee.response.v2",
  "success": true,
  "data": {
    "pack": {
      "schema": "ee.pack.v2",
      "packDna": {
        "schema": "ee.context.pack_dna.v1",
        "degraded": []
      }
    }
  },
  "degraded": []
}
```

Consumer behavior: parse the envelope first, then parse `pack.schema`, then
parse `pack.packDna.schema`. Do not assume every nested object shares the
envelope version.

Additive field example:

```json
{
  "schema": "ee.proximity.v1",
  "memoryA": "mem_a",
  "memoryB": "mem_b",
  "minCut": 0.31,
  "interpretation": "strong",
  "treePath": ["mem_a", "mem_b"],
  "degraded": []
}
```

If a future patch adds an optional `witness` field to `ee.proximity.v1`, old
agents may ignore it. If `minCut` becomes a string or `treePath` is removed, the
schema must become `ee.proximity.v2`.

## Migration Checklist

Any schema-breaking graph change must land these artifacts together:

- A new `docs/schemas/*.v2.json` file.
- A migration note that names changed fields and fallback behavior.
- A compatibility period if both old and new shapes are emitted.
- A degraded signal when command output is affected by mixed schema mode.
- Snapshot coverage for the new shape.
- Agent-facing documentation updates that include at least one worked example.
