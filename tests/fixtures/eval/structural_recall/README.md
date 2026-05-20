# fx.structural_recall.v1

`bd-bife.11` PPR pack-quality regression fixture.

This fixture pins six eval scenarios for `ee context` structural reranking:
`orphan_query`, `over_grounding`, `related_concept`, `contradicted_belief`,
`fresh_workspace`, and `derived_revision`.

The source memories are synthetic and secret-free. `structural_edges` encode the
graph evidence future PPR evaluation must consume, while
`structural_recall_expectations` points at the baseline and post-G1 comparison
snapshots under `tests/snapshots/`.
