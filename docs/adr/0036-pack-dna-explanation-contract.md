# ADR 0036: Pack DNA Explanation Contract

Status: accepted
Date: 2026-05-15
Bead: bd-fdvt.5

## Context

ADR 0007 makes context packs the primary `ee` user experience. ADR 0008
allows graph analytics to improve explanations only when those analytics stay
derived, explainable, and degraded-safe.

Pack DNA is the graph explanation block attached to
`ee context "<task>" --explain --json` under `data.pack.packDna`. It is meant
for coding agents that already trust the ordinary context pack enough to use
it, but need to understand why selected memories cluster together or why a
graph signal pulled nearby evidence into view.

The block borrows the "robot insights" style from graph tooling: expose a
small set of named, machine-parseable graph signals rather than a long
free-text explanation. Agents will key off those field names in prompts,
tests, dashboards, and downstream tooling. Unversioned drift would break those
consumers even when the pack contents remain useful.

## Decision

`ee.context.pack_dna.v1` is the public Pack DNA contract. The v1 block is
versioned by the top-level `schema` field and is defined by
`docs/schemas/ee.context.pack_dna.v1.json`.

The standard v1 shape contains four graph explanation blocks:

| Field | Meaning | Algorithmic source |
| --- | --- | --- |
| `voronoiDominator` | Selected or trusted memory whose Voronoi cell covers the largest part of the packed local evidence region | Voronoi cells over the memory-link projection |
| `communityOfMass` | Community carrying the largest share of selected pack members | Louvain-style community detection over the undirected projection |
| `egoSubgraph` | Local neighborhood around the dominator or most relevant center | Bounded-radius ego graph |
| `pprNeighbors` | High-scoring non-seed neighbors that explain structural pull near the query | Personalized PageRank over the directed projection |

The block also contains `snapshotVersion` and `degraded[]`. The snapshot
identifier tells consumers which derived graph artifact was used. The degraded
array explains partial graph failures such as missing dominators or stale graph
state without invalidating the ordinary context pack.

The field names above are frozen for v1. Adding optional fields is allowed only
when old consumers can ignore them under the schema's field-preset rules.
Renaming, removing, or changing the meaning of any standard field requires a
new `ee.context.pack_dna.v2` schema, a new ADR or ADR amendment, a migration
note in the agent-UX docs, and a deprecation period for v1.

Internal Rust structs may use implementation-oriented names, but public JSON
under `data.pack.packDna` must map to the schema names. In particular,
implementation names such as `dominator` are not the v1 API unless a v2 ADR
explicitly chooses them.

## Consequences

Agents get a stable, compact explanation surface for surprising context packs.
They can parse the four named blocks independently instead of asking an LLM to
interpret prose.

Pack DNA remains explanatory rather than authoritative. Provenance, trust
class, policy checks, and ordinary pack `degraded[]` entries still decide
whether a pack is usable. A Pack DNA degradation means the graph explanation is
incomplete; it does not mean the context pack failed.

The contract creates maintenance pressure at the renderer boundary. Graph code
can evolve, but public output must preserve `ee.context.pack_dna.v1` until a v2
contract lands. Runtime tests should fail when internal field names leak into
the public JSON block.

Snapshot caching and determinism become part of the product contract. The same
workspace, graph snapshot, query, and pack inputs must yield byte-identical
Pack DNA JSON after volatile fields are removed.

## Rejected Alternatives

- **Single free-text "why this pack" explanation**: Rejected because it is not
  machine-parseable, hard to diff, and easy for agents to over-trust as
  reasoning rather than derived graph evidence.
- **Only add PPR scores to each selected result**: Rejected because PageRank
  alone does not reveal community structure, local topology, or whether a
  trusted anchor dominates the packed evidence region.
- **Mermaid or graph-render output as the primary contract**: Rejected because
  diagrams are useful for humans but awkward for JSON-only agent consumers,
  golden tests, and field-level compatibility.
- **Use the internal Rust struct as the public schema**: Rejected because
  implementation names are free to optimize for code clarity. Public field
  names are an API and need schema/version governance.

## Verification

This decision is enforced by these artifacts:

- `docs/schemas/ee.context.pack_dna.v1.json` defines the public schema and
  required standard fields.
- `tests/snapshots/pack_dna.snap` freezes the v1 example shape.
- `tests/contracts/graph_schemas_v1.rs` checks that the schema example exists
  and includes the required Pack DNA blocks.
- `tests/graph_determinism.rs::context_pack_dna_output_is_deterministic`
  exercises real `ee context --explain --json` output three times and compares
  the serialized Pack DNA block for byte stability.
- `scripts/e2e_overhaul/g2_pack_dna.sh` is the logged e2e probe for the
  context explanation surface.

Future verification should include a runtime schema-conformance assertion that
validates `data.pack.packDna` against `ee.context.pack_dna.v1.json`; passing
only the schema example is not enough to prove public-output conformance.
