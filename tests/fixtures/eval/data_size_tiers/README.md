# data_size_tiers Evaluation Fixture

Fixture ID: `fx.data_size_tiers.v1`

Scenarios:

- `usr_context_small_workspace`
- `usr_context_medium_workspace`
- `usr_context_large_workspace`

This fixture defines deterministic small, medium, and large memory-source
profiles for context-packing evaluation. It is intentionally compact: the
source file stores stable generation profiles instead of hundreds of repeated
memory records.

The expected agent-facing signal is that `ee context ... --json` behaves
predictably as the memory set grows:

- small tier: all relevant memories fit with full provenance
- medium tier: section quotas and redundancy suppression are visible
- large tier: budget truncation is explained and provenance is preserved

Generated run artifacts belong under
`target/ee-e2e/data_size_tiers/<run-id>/`.
