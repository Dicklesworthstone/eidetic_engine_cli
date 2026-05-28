# ADR 0048: N6 persistent homology on the memory-link graph — defer to research backlog

Status: Deferred (research backlog)
Date: 2026-05-27
Bead: bd-17c65.14.6 (N6)

The design is captured here so a future research slice has a load-bearing
entry point; no implementation lands now.

## Context

The memory-link graph today is analyzed only through PageRank and the
existing centrality stack in `fnx-algorithms`. Persistent homology
would add a principled, scale-free topological feature: H0 surfaces
connected components (rule families and curation islands), H1
surfaces 1-cycles (potentially redundant or circular reasoning
chains), and the persistence diagram ranks each feature by birth /
death scale so clusters that traditional methods (silhouette, DBSCAN,
hierarchical) overweight or miss become explicit. The intent was for
`ee curate` to surface consolidation candidates from H0/H1 features
instead of relying on heuristic clustering alone.

Why this is in the research tier rather than the implementation tier:

- N6 is parented under bd-17c65.14, the "research-grade analyses on
  the memory-link graph" arc, alongside Bayesian posteriors
  (bd-17c65.14.7, shipped) and structural-decay policies
  (ADR 0035). The arc explicitly admits some items will not earn
  implementation slots because the consumer side does not yet exist.
- No current `ee curate` surface consumes a persistence-diagram
  feature; the curation candidate scoring pipeline is driven by
  PageRank + recency + harmful-feedback signals
  (`src/search/scoring.rs:DEFAULT_*`), and consolidation candidates
  flow through `src/core/curate.rs::run_consolidation_*` against
  graph-centrality + co-occurrence, not homology.
- The pre-flight investigation the bead description required (Phase 0:
  audit `fnx-algorithms` for persistent-homology / persistence-diagram
  support; branch on outcome) has no upstream payoff today: even if
  the algorithm shipped, the downstream consumer surface that would
  use H0/H1 features does not exist, so the implementation would
  immediately enter the dead-code attribution surface.
- Priority is P3 in the tracker, which is the canonical "design-only,
  defer until a consumer asks" tier for this project (P0/P1/P2 are
  implementation-bound; P3/P4 are explicitly research/backlog).

## Decision

Defer the N6 implementation to the research backlog. This ADR is the
load-bearing record of *what* persistent homology would compute on the
memory-link graph, *which* libraries are candidates, *what* the spike
scope looks like, and *what* would have to be true for the deferral
to flip to an implementation slot.

A future research slice may reopen the bead and adopt this ADR as the
design baseline; nothing here freezes that surface.

## What persistent homology would compute on the memory-link graph

Input shape: the memory-link graph projection that
`src/graph::projection::*` already exposes — directed edges between
`StoredMemory` nodes labeled by `StoredMemoryLink.relation`
(`derives_from`, `refines`, `contradicts`, `supersedes`,
`co_occurs_with`, etc.). Persistence on a graph normally requires an
undirected weighted simplicial complex; the typical translation is:

- **Step 1: Convert to undirected weighted graph.** Treat each
  directed link as an undirected edge whose weight is a function of
  `(link.confidence, link.recency, link.harmful_feedback_count)`.
  Anti-relations (`contradicts`, `supersedes`) get weighted opposite
  to support-relations (`derives_from`, `refines`). The weight
  function is part of the determinism contract — the same DB +
  scoring config must produce the same edge weights.
- **Step 2: Build the Vietoris-Rips or clique complex.** Add a
  1-simplex per edge whose filtration value is the edge weight. Add a
  2-simplex per triangle (three pairwise-connected memories) whose
  filtration value is the max edge weight in the triangle. Higher
  simplices are out of scope for the spike — H0 and H1 are the
  load-bearing features.
- **Step 3: Compute persistence diagrams.**
  - **H0 (connected components):** ranked list of `(birth, death,
    representative_memory_id)` tuples. Long-lived components are
    rule families that resist consolidation; short-lived components
    are noise-tier candidates that merge into the dominant component
    quickly.
  - **H1 (1-cycles):** ranked list of `(birth, death,
    representative_cycle)` tuples where each cycle is a sequence of
    memory ids. Long-lived cycles are structurally meaningful (e.g.,
    a refinement loop between three memories that should consolidate);
    short-lived cycles are noise.
  - H2 and higher are deferred — the cost / benefit on a memory-link
    graph at our typical scale (10K-100K memories per workspace) is
    not justified by the prospective consumer.
- **Step 4: Emit a `ee.curate.homology.v1` envelope.** Schema sketch:
  ```json
  {
    "schema": "ee.curate.homology.v1",
    "h0_components": [
      {"birth": 0.0, "death": 0.42, "representative_memory_id": "mem_..."},
      ...
    ],
    "h1_cycles": [
      {"birth": 0.21, "death": 0.67,
       "representative_cycle": ["mem_a", "mem_b", "mem_c", "mem_a"]},
      ...
    ],
    "filtration": {"weight_function": "confidence_recency_v1", "edges": 12345},
    "degraded": []
  }
  ```
- **Step 5: Surface H0/H1 features as curation candidates.** A
  consolidation candidate is opened when H0 birth-death gap exceeds a
  configured threshold (suggested default: 0.3) or H1 birth-death gap
  exceeds 0.5; both thresholds belong in
  `[curate.homology]` config with sensible defaults.

## Candidate libraries

Three Rust persistent-homology crates evaluated by author intent
only (no Cargo experiments staged from this ADR):

| Crate | Suitability | Notes |
|---|---|---|
| `gudhi` Rust bindings | Best-fit for H0/H1 + persistence diagrams; mature upstream (C++ GUDHI), wide algorithm coverage | C++ FFI through `bindgen`; conflicts with the `#![forbid(unsafe_code)]` crate policy unless gated behind a feature + a thin safe adapter |
| `phat-rs` | Pure-Rust persistent homology via the matrix-reduction algorithm (PHAT); smaller surface than GUDHI | Active but small; no published `Cargo.toml` benchmark on our typical 10K-100K-edge graph; needs a spike to confirm scaling |
| `simplicial-rs` (or equivalent pure-Rust simplicial-complex crate) | Lower-level — builds the complex, expects the consumer to drive reduction | More work for the implementer; cleanest unsafe-free path |
| Hand-rolled boundary-matrix reduction in `fnx-algorithms` | Avoids the new dep + license review; matches the existing "use franken_*" convention | The implementation work itself is 1–2 weeks; we are still paying the spike cost just on `fnx-algorithms` side |

The Phase-0 audit the original bead requested
(`fnx-algorithms` persistent-homology surface inventory) is not
performed by this ADR. The deferral is upstream of that audit: until
a consumer surface asks for H0/H1 features, the audit's branch (A: use
upstream; B: file sibling bead; C: defer) collapses to "C: defer."

## Spike scope (if the deferral ever flips)

A future implementation slice would budget roughly two engineer-weeks
broken down as:

1. **Day 1–2: Phase 0 audit + library selection.** Run the audit the
   bead description required; pick `gudhi` vs `phat-rs` vs hand-rolled
   based on Cargo dep policy + the
   `tests/forbidden_deps.rs` gate. Capture the decision as an addendum
   to this ADR.
2. **Day 3–4: Filtration design and determinism contract.** Lock the
   edge-weight function and the simplicial-complex construction step.
   Write the determinism proof: same DB + scoring config produce
   byte-identical diagrams across runs.
3. **Day 5–7: H0/H1 implementation behind a feature flag.**
   `cargo feature = "topological-curation"` gates the new dep and the
   curate-side consumer; default-off so the addition is non-breaking
   for existing builds.
4. **Day 8–9: 10K-node fixture benchmark.** Real fixture under
   `tests/fixtures/eval/` with 10K memory nodes + ~30K links; bench
   targets: H0 computation under 200ms p99, H1 under 1s p99. If
   targets miss, downgrade to H0-only for v1.
5. **Day 10: Curate-side consumer.** Open consolidation candidates
   from H0/H1 features. Gate behind `[curate.homology] enabled = false`
   default-off.
6. **Day 11–12: Test coverage.** Unit tests for the filtration math,
   contract test for the v1 schema, golden tests for the diagram on
   the fixture, property test asserting determinism across reordered
   input. Forbidden-deps gate update if a new dep landed.
7. **Day 13–14: Docs + closeout.** ADR addendum, agent-ux page, schema
   file under `docs/schemas/ee.curate.homology.v1.json`, and the
   `degraded[]` code vocabulary additions
   (`homology_unavailable`, `homology_budget_exceeded`,
   `homology_input_too_large`).

The spike output is a focused PR + ADR addendum + benchmark artifact;
nothing requires standing up new infrastructure.

### Re-open Criteria

Re-open bd-17c65.14.6 (or open a successor) when ALL of:

1. A consumer surface — `ee curate consolidate --topology`,
   `ee insights --section homology`, or equivalent — has at least one
   concrete agent harness or research user.
2. The bead priority is uplifted to P2 or above by an operator
   decision (P3 stays in the research backlog by definition).
3. The spike budget (two engineer-weeks) fits the current release
   train. Persistent homology done well costs more than the spike
   below; a partial implementation that ships only H0 with no clear
   H1 / consumer story is worse than continued deferral.
4. `fnx-algorithms` audit either shows upstream support OR a sibling
   bead is filed in the `franken_networkx` repo with credible
   pathway.

If any of (1)–(4) is unmet, the right outcome is to keep this ADR as
the design record and leave the implementation deferred.

## Non-goals

- Persistent homology on richer simplicial complexes (Čech, witness,
  alpha) is explicitly out of scope; the marginal information beyond
  Vietoris-Rips on this graph shape is small.
- Higher Betti numbers (H2 and above) are out of scope for the same
  reason; H2 on a 10K-edge graph at our typical density is dominated
  by noise.
- Streaming persistence (updating diagrams as the graph evolves) is
  research-grade-research and not part of this design.
- Visualizations (persistence-diagram plots, barcode renders) belong
  in a separate ADR if/when a consumer asks; this ADR pins only the
  data contract.

## Verification

This ADR is documentation-only; no Cargo, no schema change, no source
mutation. Static checks:

- `git diff --check -- docs/adr/0048-persistent-homology-N6.md`:
  passes.
- Cross-link sanity: the parent (bd-17c65.14), the sibling
  (bd-17c65.14.7, Bayesian posteriors, shipped), and ADR 0035
  (structural decay, the other graph-side analysis ADR) are the
  linked reference points; all reachable from this file.
- No code path references this ADR programmatically; readers find it
  through `docs/adr/README.md` index.

Refs: bd-17c65.14 (research-grade analyses parent), bd-17c65.14.7
(Bayesian posteriors, shipped sibling), ADR 0032 (Bayesian-memory
posteriors), ADR 0035 (structural-decay policy), ADR 0042
(symbol-graph derived index).
