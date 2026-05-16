# Determinism Seed Labels

Child-seed labels are part of the deterministic output contract. A call to
`Deterministic::child(label)` derives a new seed from the parent seed and the
label string, so changing a label changes the emitted IDs, tie-breaks, and any
other token-derived output for that scope.

Every production label must appear in both this document and
`src/core/determinism.rs`.

| Label | Producer call site | Consumer |
| --- | --- | --- |
| `pack.mmr_tiebreak` | `src/pack/mod.rs:assemble_mmr_draft` | MMR diversity tie-breaks during pack assembly |
| `pack.selection_id` | `src/core/context.rs:persist_pack_record` | Context pack selection and persisted pack IDs |
| `pack.skipped_order` | `src/pack/mod.rs:PackDraft::skipped_for_output` | Stable ordering for omitted or skipped candidates |
| `search.score_jitter` | `src/core/search.rs:run_search` | Deterministic score perturbation for stability tests |
| `search.canonical_ties` | `src/core/search.rs:canonicalize_equivalent_component_scores` | Stable ordering for equal-score search results |
| `search.rerank` | `src/core/search.rs:search_sync` | Rerank-stage deterministic tie-breaks |
| `ulid.memory` | `src/models/id.rs:Id::now_seeded` | Memory UUIDv7 generation |
| `ulid.audit` | `src/db/mod.rs:generate_audit_id_seeded` | Audit UUIDv7 generation |
| `ulid.workspace` | `src/core/workspace.rs:stable_workspace_id_seeded` | Workspace UUIDv7 generation |
| `ulid.pack` | `src/core/context.rs:persist_pack_record_seeded` | Context pack UUIDv7 generation |
| `clustering.kmeans_init` | `src/curate/cluster_coherence.rs:centroid_hash` | Cluster centroid hash derivation |
| `counterfactual.replay` | `src/core/lab.rs:run_counterfactual` | Counterfactual replay child seed |
| `lab.replay` | `src/core/lab.rs:replay_episode` | Lab replay child seed |

## Change Rules

1. Add new labels in `src/core/determinism.rs::SEED_LABELS` and
   `SEED_LABEL_REGISTRY`.
2. Add the same label to the table above with the production call site and the
   consuming behavior.
3. Use literal labels at `.child("...")` call sites. Dynamic labels are reserved
   for scoped suffixes that are explicitly registered by a base label.
4. Run `cargo test --test seed_labels_consistency_test`.
