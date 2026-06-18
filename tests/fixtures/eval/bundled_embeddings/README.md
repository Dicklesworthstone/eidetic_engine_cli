# bundled_embeddings Evaluation Fixture

Fixture ID: `fx.bundled_embeddings.v1`

Scenario:

- `usr_bundled_embedding_analyst_paraphrase`

This fixture pins the analyst regression that motivated the bundled embedding
track. The source memory uses finance shorthand (`RBLX`, bookings, `FCF`,
Robux) while the semantic recall case queries a faithful paraphrase with no
literal ticker or product tokens:

`video game virtual currency platform owner cash generation`

The fixture carries two signals:

- ordinary eval queries with direct lexical overlap, so `ee eval run` remains a
  stable retrieval-fixture report
- `semantic_recall_expectations`, which compares a deterministic hash-baseline
  retrieval order against the neural semantic order and requires a positive
  recall gain

Generated run artifacts belong under
`target/ee-e2e/bundled_embeddings/<run-id>/`.
