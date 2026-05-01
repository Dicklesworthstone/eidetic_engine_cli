# metamorphic_evaluation Evaluation Fixture

Fixture ID: `fx.metamorphic_evaluation.v1`

Scenario:

- `usr_eval_metamorphic_memory_regressions`

This fixture pins metamorphic evaluation behavior for paired memory states. It
covers five deterministic relation families:

- positive feedback strengthens the same selected rule
- contradictory evidence raises review risk instead of becoming authoritative
- supersession prefers the latest procedure and excludes the old procedure
- tighter token budgets preserve the highest-priority memory while trimming detail
- semantic fallback emits `semantic_disabled` and preserves lexical top results

Generated run artifacts belong under
`target/ee-e2e/metamorphic_evaluation/<run-id>/`.
