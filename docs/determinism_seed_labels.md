# Determinism Seed Labels

Child-seed labels are part of the deterministic output contract. A call to
`Deterministic::child(label)` derives a new seed from the parent seed and the
label string, so changing a label changes the emitted IDs, tie-breaks, and any
other token-derived output for that scope.

Every production label must appear in both this document and
`src/core/determinism.rs`.

| Label | Producer call site | Consumer |
| --- | --- | --- |
| `pack.mmr_tiebreak` | `src/core/pack.rs:assemble_pack` | MMR diversity tie-breaks during pack assembly |
| `pack.selection_id` | `src/core/pack.rs:persist_pack_record` | Context pack selection and persisted pack IDs |
| `pack.skipped_order` | `src/core/pack.rs:skipped_candidates` | Stable ordering for omitted or skipped candidates |
| `search.score_jitter` | `src/core/search.rs:run_search` | Deterministic score perturbation for stability tests |
| `search.canonical_ties` | `src/core/search.rs:canonicalize_result_ties` | Stable ordering for equal-score search results |
| `search.rerank` | `src/core/search.rs:rerank_top_k` | Rerank-stage deterministic tie-breaks |
| `ulid.memory` | `src/db/id_gen.rs:next_memory_id` | Memory UUIDv7 generation |
| `ulid.audit` | `src/db/id_gen.rs:next_audit_id` | Audit UUIDv7 generation |
| `ulid.workspace` | `src/db/id_gen.rs:next_workspace_id` | Workspace UUIDv7 generation |
| `ulid.pack` | `src/core/pack.rs:pack_id` | Context pack UUIDv7 generation |
| `clustering.kmeans_init` | `src/curate/clustering.rs` | Cluster centroid initialization |
| `counterfactual.replay` | `src/core/lab.rs:counterfactual_replay` | Counterfactual replay child seed |
| `lab.replay` | `src/core/lab.rs:replay` | Lab replay child seed |

## Change Rules

1. Add new labels in `src/core/determinism.rs::SEED_LABELS` and
   `SEED_LABEL_REGISTRY`.
2. Add the same label to the table above with the production call site and the
   consuming behavior.
3. Use literal labels at `.child("...")` call sites. Dynamic labels are reserved
   for scoped suffixes that are explicitly registered by a base label.
4. Run `cargo test --test seed_labels_consistency_test`.
