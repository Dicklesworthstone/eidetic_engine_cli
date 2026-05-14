# Schema Evolution Policy

Graph-accretion schemas are stable public contracts once advertised by
`ee::core::supported_schemas()`.

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
