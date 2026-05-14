# Graph Configuration

Graph configuration keys live under the `[graph.*]` TOML namespace. They move
graph-theoretic feature thresholds out of algorithm code so workspace owners can
tune behavior without rebuilding `ee`.

The defaults below are the built-in values surfaced by the merged config show
report. Project and user config files may override them through the normal
configuration precedence chain.

| Key | Type | Default | Range | Description | Tuning Advice |
| --- | --- | --- | --- | --- | --- |
| `graph.ppr.alpha` | float | `0.30` | `0.0..=1.0` | Weight reserved for Personalized PageRank influence when a graph-aware context ranker blends text and graph scores. | Lower toward `0.0` to preserve legacy text-first ordering; raise only after graph links are trusted and dense. |
| `graph.health.contradiction_threshold` | float | `0.20` | `0.0..=1.0` | Minimum contradiction-cluster density required before graph health surfaces treat a community as suspicious. | Raise in noisy workspaces with many tentative contradiction edges; lower when contradiction links are curated and high-confidence. |
| `graph.curate.onion_decay_max` | float | `3.0` | `> 0.0` | Maximum structural decay multiplier for memories far from the graph core. | Increase when peripheral memories should fade more aggressively; keep near `1.0` for small or sparse workspaces. |
| `graph.curate.articulation_protection_multiplier` | float | `0.5` | `0.0..=1.0` | Decay multiplier applied to articulation memories so bridge facts are protected from ordinary structural decay. | Lower to protect bridge memories more strongly; raise toward `1.0` when articulation points are too sticky. |
| `graph.hits.profile_boost` | float | `0.5` | `>= 0.0` | Extra weight for HITS authority/hub scores when a context profile asks for orientation or grounding. | Increase for navigation-heavy contexts; lower when HITS scores overwhelm direct textual relevance. |
| `graph.causal.min_cost_normalization` | float | `1.0` | `> 0.0` | Denominator used to normalize causal min-cost explanations into stable unit-ish scores. | Tune only with a fixture: too low exaggerates weak causal paths, too high hides useful causal evidence. |
| `graph.pack_dna.max_items` | integer | `10` | `>= 0` | Maximum memory nodes included in a Pack DNA explanation block. | Raise for debugging and audits; keep small for prompt-budgeted agent context. |
| `graph.pack_dna.max_edges` | integer | `30` | `>= 0` | Maximum graph edges included in a Pack DNA explanation block. | Raise when agents need richer local topology; lower when JSON size matters more than topology detail. |
| `graph.gomory_hu.sample_threshold` | integer | `500` | `>= 0` | Node-count threshold above which Gomory-Hu style proximity work should use deterministic sampling instead of exact computation. | Lower on constrained hosts; raise on large hosts when exact cuts are affordable. |
| `graph.gomory_hu.sample_size` | integer | `100` | `>= 0` | Deterministic pivot/sample size used for large-graph Gomory-Hu approximations. | Increase for better approximation quality; lower to bound latency in agent hot paths. |

Example:

```toml
[graph.ppr]
alpha = 0.0

[graph.gomory_hu]
sample_threshold = 250
sample_size = 64
```

Setting `graph.ppr.alpha = 0.0` is the compatibility profile for pack assembly:
the graph rank contribution is disabled and text-first ordering should remain
byte-identical once the PPR integration lands.
