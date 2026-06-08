# Causal-Ancestry PPR Pre-Warming

A serial swarm has no centralized planner, but the beads dependency graph already
records which task came before which. When a query names task/bead IDs, `ee` can
seed Personalized PageRank from those nodes, walk **backward** along causal
`depends_on`/`blocks` edges, and surface the hard-won lessons of **upstream**
tasks — cross-task continuity without a planner takeover. It stays squarely in
the retrieval lane: an explainable, capped, additive boost on top of the base
frankensearch ranking.

Bead lineage: `bd-1n0np.19` (feature), `19.1` (seed PPR from query IDs + backward
traversal), `19.2` (caps + explainability + graceful no-op), `19.3` (tests),
`19.4` (e2e), `19.5` (docs), `19.6` (perf budget).

## How it works (`src/graph/causal.rs`)

The whole path reuses the existing fnx graph machinery — **no new graph/PPR
engine, no petgraph**:

1. **Extract seeds** — `extract_causal_seed_ids(query)` parses `bd-…` bead IDs
   from the query (deduped, sorted). No reliable IDs → no seeds → clean no-op.
2. **Backward causal traversal** — `causal_ancestry_ppr_seed_map(graph, seeds,
   CausalPprSeedConfig)` walks backward via the existing `compute_causal_ancestry`
   and weights each upstream ancestor by `ancestor_decay^path_length` (nearer
   lessons dominate), capped by `max_seeds` / `max_ancestors_per_seed`. The result
   is a `BTreeMap` seed map.
3. **Run PPR** — the caller feeds that seed map to the existing
   `graph::ppr::compute_personalized_pagerank_result(graph, seed_map)` (with the
   PPR prefetch cache).
4. **Capped additive boost** — `compute_causal_ppr_boosts(ppr_scores,
   CausalBoostConfig)` turns the PPR scores into a bounded additive boost per
   memory: normalized to the top score and scaled to `max_boost` (a hard
   ceiling), with candidates below `min_ppr_score` left unboosted. The base
   ranking stays frankensearch's; the boost only lifts upstream-lesson memories.

## Three hard invariants

1. **Retrieval lane, never a planner.** The boost is *additive and capped*
   (`max_boost` is a hard ceiling) on top of the base frankensearch ranking — a
   high PPR mass can never override base retrieval order.
2. **The seed never diffuses into noise.** No reliable query IDs → graceful no-op;
   PPR scores below `min_ppr_score` are not boosted; hop/seed/ancestor caps bound
   the spread.
3. **Never silent.** `CausalBoostResult.status` reports `applied` vs
   `skipped_no_seeds` vs `skipped_all_below_threshold`, plus considered/boosted
   counts — and the ancestry path is meant to be cited in `why`/PackDna so a boost
   is always explainable.

## Status

- **Landed + verified:** the seed-computation core (`19.1`,
  `extract_causal_seed_ids` + `causal_ancestry_ppr_seed_map`) and the capped
  additive boost + no-op/explainability accounting (`19.2`,
  `compute_causal_ppr_boosts`), with inline unit tests (`19.3`): decaying
  ancestor weights, caps, graceful no-op, hard boost ceiling, determinism.
- **Follow-on (integration):** wiring the seed map → `personalized_pagerank` →
  boost into the live `ee pack`/retrieval hot path with the PPR prefetch cache,
  citing the ancestry path in `ee why` / PackDna, the e2e
  (`scripts/e2e_causal_ppr.sh`, `19.4`), and the perf budget (`19.6`, bounded
  PPR-on-pack latency, warm vs cold cache).

## See also

- [`dueling-wizards-why-packdna-signals.md`](dueling-wizards-why-packdna-signals.md) — where the causal-ancestry citation surfaces.
- [`dueling-wizards-contradiction.md`](dueling-wizards-contradiction.md) — the other graph-reuse retrieval signal (k-truss/Louvain over the conflict graph).
