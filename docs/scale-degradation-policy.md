# Graph Scale Degradation Policy

Tracking bead: `bd-bife.17`

The scale fixtures under `tests/fixtures/scale/` model graph workloads at
10k, 50k, and 100k memories without materializing those rows during normal CI.
Each fixture is a deterministic JSONL spec that pins the seed family, memory
count, edge count, hash embedder profile, and stable generated ID range.

`src/graph/scale_policy.rs` is the executable policy for deciding whether an
algorithm can run exactly at a given graph shape. The policy keeps a 100k-memory
`ee insights` bundle within a 5s planning budget by skipping or capping
algorithms whose asymptotic cost would dominate the command.

| Algorithm | 100k policy | Degraded code |
| --- | --- | --- |
| Personalized PageRank | run exact | none |
| HITS | run exact | none |
| PageRank | run exact | none |
| Betweenness | deterministic pivot sample | `graph_scale_pivot_sampled` |
| Communicability betweenness | deterministic pivot sample | `graph_scale_pivot_sampled` |
| K-truss | run exact | none |
| Louvain | run exact | none |
| Onion layers | run exact | none |
| Articulation points | run exact | none |
| Gomory-Hu | skip above 2,000 nodes | `graph_scale_gomory_hu_skipped` |
| Voronoi cells | run exact | none |
| Ego graph | run exact | none |
| Transitive closure | cap traversal depth to 10 | `causal_depth_capped` |
| Min-cost flow | cap iterations | `graph_scale_min_cost_flow_iteration_capped` |
| Dominance frontiers | run exact | none |
| All-pairs LCA | lazy pair query above 1,000 nodes | `graph_scale_all_pairs_lca_lazy` |
| SimRank | deterministic Jaccard fallback above 500 nodes | `graph_scale_simrank_jaccard_fallback` |

The policy is intentionally pure and allocation-free so callers can consult it
before building a graph projection. Runtime command integration should lower
the returned degraded code into the response-local `degraded[]` array for the
affected section and should include the policy reason in the repair or details
field. Fixture and policy conformance is covered by
`tests/graph_scale_degradation.rs`; policy overhead is tracked by
`benches/graph_scale_policy.rs`.
