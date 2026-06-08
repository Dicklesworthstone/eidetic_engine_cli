# Contradiction Operationalization — Explicit-Evidence Detection

`ee` surfaces contradictions between memories so a pack never silently ships two
memories that disagree. The discipline both design wizards agreed on is
**explicit-evidence-FIRST**: v1 detection reaches only memories that already
carry conflict *structure* in the store. The fuzzy embedding-opposition detector
is the false-positive-prone part and stays opt-in until proven.

Bead lineage: `bd-1n0np.7` (feature), `7.1` (ADR + `ConflictCluster` model),
`7.2` (detect from explicit DB evidence, reusing `graph::health` clusters),
`7.3` (`ee conflict list/explain/cluster` + `ee curate contradictions`), `7.4`
(audited resolution), `7.5` (pack guard + forced mode), `7.6` (tests), `7.8`
(docs/capabilities/help).

## The six explicit signals

A [`ConflictEdge`](../../src/core/contradiction_detect.rs) is one explicit,
DB-recorded conflict relationship between two memories — never an inferred guess.
Each signal carries a load-bearing weight (milli-units): how strongly it
implicates a genuine contradiction.

| Signal (`ExplicitConflictSignal`) | Weight | Evidence the store already holds |
|---|---:|---|
| `contradiction_link` | 1000 | a direct `contradicts` memory link |
| `supersession` | 900 | one memory supersedes the other |
| `duplicate_divergent` | 700 | near-duplicate content that nonetheless diverges |
| `validity_window_overlap` | 600 | validity windows overlap while asserting different things |
| `trust_outcome_split` | 500 | trust / outcome evidence points in opposite directions |
| `repeated_co_selection` | 300 | repeatedly co-selected into the same packs |

The heaviest signals (`contradiction_link`, `supersession`) are sparse, explicit
graph links; the lighter ones are derived from existing rows. A pair backed by
several signals keeps the **heaviest** weight seen for that pair.

## How detection works

`detect_explicit_contradictions(edges, config)` is pure and deterministic:

1. **Canonicalize + dedup.** Each edge is reduced to an unordered, trimmed
   `(low, high)` pair; blanks and self-loops are dropped. Duplicate pairs collapse
   to one, keeping the maximum signal weight. `explicit_edge_count` is the number
   of distinct surviving pairs.
2. **Build the conflict graph.** Nodes are memories; edges are the deduped pairs —
   the *same* graph construction `graph::health` uses for its `Contradicts`
   relation graph.
3. **Reuse the proven cluster detector.** `detect_contradiction_clusters_with_policy`
   (k-truss + Louvain) finds clusters; the density threshold is overridable via
   `ContradictionDetectionConfig::density_threshold`.
4. **Rank.** Each cluster gets a deterministic composite `rank_score`:
   `severity_factor × density × (centrality + 1) × (1 + load_bearing/1000)`, where
   `centrality` is conflict-edge degree summed over exemplar members and
   `load_bearing_milli` is the weight mass of incident edges. Severity multiplies
   (`Incoherent` ×2.0, `Inconsistent` ×1.0). Clusters sort most-urgent first, with
   `louvain_id` breaking ties.

The result is a `ContradictionDetectionReport { clusters, explicit_edge_count,
fuzzy_near_conflict_skipped }`.

## Two hard invariants

1. **Explicit-evidence-FIRST; no silent widening.** The fuzzy near-conflict pass
   is deferred in v1: requesting it
   (`ContradictionDetectionConfig::include_fuzzy_near_conflict = true`) sets
   `fuzzy_near_conflict_skipped = true` on the report rather than running the
   false-positive-prone path. The omission is always surfaced, never silent.
2. **Deterministic + order-independent.** Edges are canonicalized and
   deduplicated and all maps are ordered, so the same edge set in any order yields
   an identical report (and identical `rank_score` ordering).

## Status (v1)

- **Landed:**
  - the explicit-evidence detection core (`src/core/contradiction_detect.rs`)
    + its property/contract tests (`tests/contradiction_detect_properties.rs`,
    `bd-1n0np.7.6`);
  - the DB-evidence *gather* that extracts the explicit signals from the store
    into `ConflictEdge`s (`bd-1n0np.7.2`);
  - the read-only `ee conflict list/explain/cluster` surface
    (`src/cli/conflict.rs`, `bd-1n0np.7.3`);
  - the pack-guard *decision core* (`src/core/contradiction_guard.rs`,
    `bd-1n0np.7.5`): `decide_contradiction_survivor` (keep higher-trust → fresher
    → deterministic id; never drop both), `unresolved_contradiction_pairs`
    (detected minus 7.4 resolutions), and `forced_contradiction_view` (ranked +
    capped, total reported so the cap is never a silent drop).
- **In flight / follow-on:** audited resolution (`bd-1n0np.7.4`); threading the
  pack-guard decision into pack assembly + the opt-in `forced` mode
  (`bd-1n0np.7.5` integration); and the `ee conflict` golden/contract tests +
  capabilities/help registration (`bd-1n0np.7.6` / `7.8`).

## See also

- [`store-integrity.md`](store-integrity.md) — write-immune + read-fence integrity that keeps the conflict graph honest.
- [`why-not.md`](why-not.md) — explaining why a memory was or was not selected, including contradiction holds.
