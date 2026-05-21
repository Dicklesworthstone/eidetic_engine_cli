# MCP Graph Tools

The MCP stdio adapter exposes graph-derived read surfaces as read-only tools.
Each tool returns the same JSON produced by the CLI unless noted.

| MCP tool | CLI surface | Required params | Return payload |
| --- | --- | --- | --- |
| `ee_insights` | `ee insights --json` | none | `ee.response.v2` with `data.schema = ee.insights.v1` |
| `ee_proximity` | `ee proximity --json` | `memoryIdA`, `memoryIdB` | `ee.response.v2` with `data.schema = ee.proximity.v1` |
| `ee_pack_dna_explain` | `ee context --explain --json` | `query` | `data.pack.packDna` only (`ee.context.pack_dna.v1`) |
| `ee_revision_impact` | `ee memory revise --dry-run --json` | `memoryId` | `data.impactAnalysis` only (`ee.memory.impact_analysis.v1`) |

All four tools advertise MCP `readOnlyHint=true`, `destructiveHint=false`,
and no `eeEffect`. `ee_revision_impact` uses a deterministic dry-run revision
probe so the impact analysis follows the existing memory-revision code path
without persisting a new revision.
