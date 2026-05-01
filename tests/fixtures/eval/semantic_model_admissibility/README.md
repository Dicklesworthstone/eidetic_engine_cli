# semantic_model_admissibility Evaluation Fixture

Fixture ID: `fx.semantic_model_admissibility.v1`

Scenario:

- `usr_semantic_model_budget_guard`

This fixture pins semantic-model admission behavior for local-first retrieval.
It covers four deterministic branches:

- a local hash embedder admitted for semantic retrieval
- a disabled model downgraded to lexical fallback with `semantic_disabled`
- an oversized local model downgraded by dimension, token, size, and latency budgets
- a remote nondeterministic backend rejected by local-first policy

Generated run artifacts belong under
`target/ee-e2e/semantic_model_admissibility/<run-id>/`.
