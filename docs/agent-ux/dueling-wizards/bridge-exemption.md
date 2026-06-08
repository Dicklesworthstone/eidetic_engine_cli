# Graph-Protected Causal Bridge Exemption

Some memories are rarely retrieved but catastrophic to lose — the single note
that bridges a failure to its disaster-recovery fix. Plain usage/recency decay
would prune exactly these. The bridge exemption protects them by reading the
*graph structure*: a memory that is the **sole articulation point** (cut vertex)
connecting otherwise-separate regions of the causal graph earns a reduced decay
multiplier, so it resists pruning even when it is seldom read.

Bead lineage: `bd-1n0np.20` (feature), `20.1`/`20.2` (graph-quality guards +
exemption cap + honest degradation), `20.3` (tests), `20.5`
(docs/capabilities). Builds on ADR-0035 (structural decay policy).

## How it works (`src/graph/decay.rs`)

`StructuralDecayIndex::adjustment(memory_id)` returns a
`StructuralDecayMultiplier { is_articulation_point, structural_multiplier,
rationale }`:

- **Articulation-point detection** — `compute_articulation_points` finds cut
  vertices (sole bridges). A memory that is the *only* path between two parts of
  the causal graph is load-bearing.
- **Structural multiplier** — a sole bridge gets an `articulation_multiplier < 1.0`
  (combined with the onion-layer multiplier), so its effective decay slows: the
  memory is protected, not exempted from accounting. The `rationale` explains why.
- **Graph-quality guards + cap (`20.2`)** — `StructuralDecayPolicy` bounds the
  protection so the exemption cannot run away, and degrades **honestly** when the
  graph is unsuitable.

## Two hard invariants

1. **Protect only *genuine* sole bridges.** A leaf is not a bridge and earns no
   protection; in a dense clique there are no cut vertices, so nothing is exempted
   — an honest miss in dense graphs, never over-protection. (Tests:
   `bridge_exemption_protects_only_genuine_sole_bridges_not_leaves`,
   `bridge_exemption_skipped_in_dense_clique_no_cut_vertex`.)
2. **Capped + honest, never a free pass.** The exemption is a bounded decay
   *multiplier* under a policy cap (`20.2`), not an unkillable flag, and it
   degrades honestly when articulation analysis can't run — deterministic
   throughout.

## Status

- **Landed + verified:** the articulation-point / structural-decay exemption core
  (`src/graph/decay.rs`) with the guards + cap (`20.2`) and unit coverage (`20.3`,
  e.g. `bridge_exemption_detects_sole_articulation_bridge`,
  `bridge_exemption_adjustment_is_deterministic`).
- **Follow-on:** capabilities/agent-docs registration of any exemption insights
  surface (golden-gated).

## See also

- [`../adr/0035-structural-decay-policy.md`](../adr/0035-structural-decay-policy.md) — the structural decay policy this exemption extends.
- [`dueling-wizards-causal-ppr.md`](dueling-wizards-causal-ppr.md) — the other graph-structure retrieval signal over the causal graph.
