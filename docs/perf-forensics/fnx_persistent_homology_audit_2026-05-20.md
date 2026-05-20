# FrankenNetworkX persistent-homology audit - 2026-05-20

Owner: bd-17c65.14.6 (N6). Manual Phase 0 audit by DustyElm.

- Generated at: `2026-05-20T16:50:15Z`
- ee checkout: `/Users/jemanuel/projects/eidetic_engine_cli`
- FrankenNetworkX checkout: `/data/projects/franken_networkx`
- Audited crates: `fnx-algorithms`, `fnx-classes`, `fnx-runtime`,
  `fnx-convert`
- Outcome: **Branch B** - `fnx-algorithms` has useful graph primitives, but
  no persistent-homology or persistence-diagram API.

## Evidence

Commands used:

```bash
rg -n "persistent|persistence|homology|barcode|vietoris|rips|simplicial|simplex|filtration|boundary matrix|tda" \
  /data/projects/franken_networkx/crates/fnx-algorithms \
  /data/projects/franken_networkx/crates/fnx-classes \
  /data/projects/franken_networkx/crates/fnx-runtime \
  /data/projects/franken_networkx/crates/fnx-convert

rg -n "pub fn (connected_components|number_connected_components|cycle_basis|minimum_cycle_basis|simple_cycles|find_cliques|k_clique_communities|gomory_hu_tree|biconnected_components|articulation_points|bridges)" \
  /data/projects/franken_networkx/crates/fnx-algorithms/src/lib.rs

rg -n "persistent homology|persistence diagram|phat|gudhi|tda|Vietoris|Rips" \
  /data/projects/franken_networkx
```

The targeted crate search found no persistent-homology terms in
`fnx-algorithms`, `fnx-runtime`, or `fnx-convert`. The only matches under the
audited crate set were unrelated sparse snapshot test names in `fnx-classes`.

The broader repository search found planning text in
`REALITY_CHECK_BRIDGE_PLAN_2026-04-08.md` and a Python script,
`scripts/compute_graph_fingerprints.py`, that describes graph invariants as a
"simplified persistent homology approach". That script is not a Rust
`fnx-algorithms` API and does not emit persistence diagrams or barcodes.

`fnx-algorithms/Cargo.toml` also has no TDA-oriented dependency. Its current
dependencies are local FNX crates plus `mwmatching`, `mt19937`, `serde`,
optional `dhat`, and `rand_core`.

## Existing reusable pieces

`fnx-algorithms/src/lib.rs` already exposes deterministic graph primitives
that are useful building blocks or baselines:

- `connected_components` and `number_connected_components` at lines 1942 and
  2055: usable for H0 at a single threshold.
- `articulation_points`, `bridges`, and `biconnected_components` at lines
  4504, 4519, and 21783: useful connectivity diagnostics.
- `cycle_basis`, `minimum_cycle_basis`, and `simple_cycles` at lines 11843,
  11739, and 19928: usable for single-threshold H1-like graph diagnostics.
- `find_cliques`, `find_cliques_recursive`, and `k_clique_communities` at
  lines 9893, 22569, and 31516: useful for clique-derived complex
  construction or comparison, but not a persistence engine.
- `gomory_hu_tree` at line 30220: useful proximity/min-cut infrastructure,
  but unrelated to persistence-pair extraction.

These functions operate on a realized graph. They do not define a filtration
over edge weights, construct a Vietoris-Rips or clique complex over threshold
levels, or pair births and deaths across thresholds.

## Missing surface

N6 should not proceed as Branch A because the following pieces are absent from
`fnx-algorithms`:

- A public `PersistenceDiagram` or barcode output type.
- A filtration type with stable ordering for vertices, edges, and higher
  simplices.
- A Vietoris-Rips or clique-complex builder parameterized by edge weight or
  distance.
- H0 persistence over sorted edge insertions with deterministic union-find
  birth/death output.
- H1 persistence pairing through boundary-matrix reduction or an equivalent
  deterministic cycle-pairing algorithm.
- Tests that freeze empty, disconnected, complete, two-cluster, and ring graph
  behavior.
- A benchmark that bounds 1K-node memory-link graph behavior.

## Recommended path

Use Branch B before implementing the ee-facing N6 command surface.

File a sibling FrankenNetworkX bead:

> Add deterministic H0/H1 persistence-diagram support to `fnx-algorithms`.

Suggested acceptance criteria:

- Add pure Rust, deterministic `fnx-algorithms` structs for persistence
  diagrams, intervals, dimensions, and filtration items.
- Accept a weighted undirected graph and a weight/distance attribute name.
- Emit H0 intervals from a stable sorted-edge union-find pass.
- Emit H1 intervals for clique-complex or Vietoris-Rips dimension 1 using a
  deterministic boundary-reduction implementation.
- Define stable tie-breaking for equal weights and equal simplex dimensions.
- Cover empty graph, disconnected graph, complete graph, two clusters, and a
  single ring in unit tests.
- Avoid `petgraph`, Tokio, and any dependency that violates ee's dependency
  policy.

After that upstream surface exists, N6 in ee can stay small:

1. Project the memory-link graph into the required FNX weighted graph shape.
2. Call the new `fnx-algorithms` persistence API.
3. Map high-persistence H0/H1 intervals into `ee curate` cluster candidates.
4. Add J6 failure fixtures for too-small and timeout cases.
5. Freeze deterministic JSON/golden output for the J2 corpus.

Branch C, vendoring a minimal implementation in ee, should remain a fallback
only if the FNX sibling work is rejected or blocked. If Branch C is used, land
an ADR first because the dependency boundary would move topology algorithms
into ee instead of the graph layer that already owns graph analytics.

## Verification

This was a static audit only. No Cargo verification was run because the change
is documentation and the required Phase 0 output is a source-read report, not a
Rust implementation.
