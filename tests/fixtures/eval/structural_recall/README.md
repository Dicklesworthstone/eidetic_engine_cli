# fx.structural_recall.v1

`bd-bife.11` PPR pack-quality regression fixture.

This fixture pins six eval scenarios for `ee context` structural reranking:
`orphan_query`, `over_grounding`, `related_concept`, `contradicted_belief`,
`fresh_workspace`, and `derived_revision`.

The source memories are synthetic and secret-free. `structural_edges` encode the
graph evidence future PPR evaluation must consume, while
`structural_recall_expectations` points at the baseline and post-G1 comparison
snapshots under `tests/snapshots/`.

Edge relations must be values that storage accepts (`MemoryLinkRelation`:
`supports`, `contradicts`, `derived_from`, `supersedes`, `related`, `co_tag`,
`co_mention`), because the eval seeds them into a real store.
`tests/eval_fixtures.rs` enforces this. The fixture originally used `cites` (two
edges) and `incident_supports` (one edge). Storage rejects both, so the family
could not execute (`bd-mv2c4`). All three edges were remapped to `supports`,
with their original endpoints and weights. A distinct citation relation would be
a product feature, not a fixture change.

Memory `trust_class` values must be stored `TrustClass` values too (a CHECK
constraint): `human_explicit`, `peer_human_attested`, `agent_validated`,
`agent_assertion`, `cass_evidence` and `legacy_import`. The fixture used
`agent_observed` (5 memories) and `agent_inferred` (1). Once the relation fix
let seeding proceed, storage rejected them. All six are now `agent_assertion`
("agent assertion, no outcome events yet"), which keeps them below the four
`human_explicit` memories, as before. The same test enforces this.
