# Graph CLI Flags

This reference aggregates the graph-related flags that agents are most likely
to combine across context packing, insights, maintenance, and graph inspection.
It is derived from the current Clap surfaces in `src/cli/mod.rs` and
`src/cli/insights/mod.rs`.

## Global Flags

These flags are accepted by every command:

| Flag | Values | Default | Use |
| --- | --- | --- | --- |
| `--workspace <PATH>` | filesystem path | current workspace discovery | Selects the workspace whose `.ee/` store and graph-derived assets are used. |
| `--json` / `-j` | boolean | false | Emits machine-readable JSON when the command supports it. |
| `--robot` | boolean | false | Uses agent-oriented output defaults; currently implies JSON where supported. |
| `--format <FORMAT>` | `human`, `json`, `toon`, `jsonl`, `compact`, `hook`, `markdown`, `mermaid` | `human` | Selects the renderer. Graph exports and Mermaid-style outputs should use explicit formats. |
| `--fields <PRESET|FIELD_LIST>` | preset or comma-separated canonical fields | `standard` | Narrows or expands JSON fields for agent consumers. |
| `--cards <LEVEL>` | `none`, `summary`, `math`, `full` | `math` | Controls card verbosity for human-oriented renderers. |
| `--schema` | boolean | false | Prints the JSON schema for the response envelope and exits. |
| `--schema-version <VERSION>` | `v0`, `v1` | `v1` | Selects the response envelope schema version. |
| `--meta` | boolean | false | Includes additional response-envelope metadata. |

## Context And Pack Flags

`ee context "<task>"` is the main graph-aware retrieval surface. `ee pack` and
`ee pack build` share most pack assembly flags, except `ee context` currently
owns `--ppr-weight`, `--explain`, and `--no-pack-dna`.

| Command | Flag | Values | Default | Use |
| --- | --- | --- | --- | --- |
| `ee context` | `--max-tokens`, `-t` | integer | `4000` | Sets the context pack token budget. |
| `ee context`, `ee pack`, `ee pack build` | `--candidate-pool` | integer | `100` for `context`; query-file/default for `pack` | Caps candidates retrieved before packing. |
| `ee context`, `ee pack`, `ee pack build` | `--speed` | `instant`, `default`, `quality` | `default` for `context`; query-file/default for `pack` | Selects retrieval speed versus quality budget. |
| `ee context`, `ee pack`, `ee pack build` | `--profile`, `-p` | `compact`, `balanced`, `thorough`, `submodular` | `balanced` for `context`; query-file/default for `pack` | Selects the context profile and section quota strategy. |
| `ee context` | `--ppr-weight <WEIGHT>` | float; clamped to `0.0..=1.0` | omitted | Blends Personalized PageRank graph pull into context ranking. |
| `ee context`, `ee pack`, `ee pack build` | `--pack-profile <PROFILE>` | `lean`, `standard`, `verbose` | `standard` for `context`; query-file/default for `pack` | Controls optional pack metadata volume. |
| `ee context`, `ee pack`, `ee pack build` | `--resource-profile <PROFILE>` | `lean`, `standard`, `swarm_heavy` | `standard` for `context`; query-file/default for `pack` | Selects pack assembly SLOs and resource assumptions. |
| `ee context`, `ee pack`, `ee pack build` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads the source-of-truth memory and graph-link tables. |
| `ee context`, `ee pack`, `ee pack build` | `--index-dir <PATH>` | filesystem path | `<workspace>/.ee/index/` | Reads derived search indexes before graph-aware packing. |
| `ee context` | `--explain` | boolean | false | Adds graph-derived Pack DNA metadata to JSON output. |
| `ee context` | `--no-pack-dna` | boolean | false | Suppresses `data.pack.packDna` even when `--explain` is set. |
| `ee context`, `ee pack`, `ee pack build` | `--no-coverage-fill[=BOOL]` | optional boolean | false | Disables the coverage-fill pass; pass `--no-coverage-fill=false` to override a lean profile. |
| `ee context`, `ee pack`, `ee pack build` | `--no-rendered-text[=BOOL]` | optional boolean | false | Suppresses rendered pack text in JSON output. |
| `ee context`, `ee pack`, `ee pack build` | `--no-skipped[=BOOL]` | optional boolean | false | Suppresses omitted/skipped item explanations. |
| `ee context`, `ee pack`, `ee pack build` | `--no-meta[=BOOL]` | optional boolean | false | Suppresses pack metadata. |
| `ee pack` | `--coordination-snapshot <PATH>` | JSON file path | omitted | Embeds a redacted coordination snapshot in the pack. |
| `ee pack` | `--coordination-stale-after-ms <MS>` | integer milliseconds | package default | Marks coordination sources stale after the configured age. |
| `ee pack` | `--include-non-affecting-degradations[=BOOL]` | optional boolean | false | Keeps non-affecting degraded signals in `data.degraded[]`. |
| `ee pack`, `ee pack build` | `--as-of <RFC3339>` | timestamp | now | Replays validity-window filtering at a deterministic time. |
| `ee pack`, `ee pack build` | `--include-expired` | boolean | false | Includes memories whose `valid_to` is before the reference time. |
| `ee pack`, `ee pack build` | `--include-future` | boolean | false | Includes memories whose `valid_from` is after the reference time. |
| `ee pack`, `ee pack build` | `--include-stale` | boolean | false | Includes memories marked with stale validity status in index metadata. |

Example:

```bash
ee context "prepare release" --workspace . --profile thorough \
  --ppr-weight 0.5 --explain --json
```

## Insights And Narrow Graph Questions

| Command | Flag | Values | Default | Use |
| --- | --- | --- | --- | --- |
| `ee insights` | `--section <NAME>` | section name | omitted | Emits one deterministic insights section, such as `bridges`, `proximityHotspots`, `knowledgeSkyline`, `hubs`, or `authorities`. |
| `ee insights` | `--explain <MEMORY_ID>` | memory ID | omitted | Frames the insights bundle around one memory explanation target. Conflicts with `--section`. |
| `ee insights` | `--limit <N>` | integer | `10` | Caps items returned for `--section`; capped internally at 100. |
| `ee insights` | `--offset <N>` | integer | `0` | Skips section items before returning the page. |
| `ee proximity <A> <B>` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads memory links for pairwise Gomory-Hu proximity. |
| `ee proximity <A> <B>` | `--min-weight <WEIGHT>` | `0.0..=1.0` | omitted | Excludes memory links below the weight floor. |
| `ee proximity <A> <B>` | `--min-confidence <CONFIDENCE>` | `0.0..=1.0` | omitted | Excludes memory links below the confidence floor. |
| `ee proximity <A> <B>` | `--link-limit <COUNT>` | integer | omitted | Caps memory links processed for graph construction. |
| `ee proximity <A> <B>` | `--include-tombstoned` | boolean | false | Includes tombstoned memory nodes in graph computation. |
| `ee why <MEMORY_ID>` | `--causal-explain` | boolean | false | Adds a `causalExplanation` block with causal ancestry and min-cost path evidence. |
| `ee why <MEMORY_ID>` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads causal graph evidence for `--causal-explain`. |
| `ee why <MEMORY_ID>` | `--confidence-threshold <THRESHOLD>` | `0.0..=1.0` | `0.5` | Filters causal explanation edges below the confidence floor. |
| `ee health` | `--robot-insights` | boolean | false | Emits graph-derived structural health for robot consumers. |
| `ee status` | `--skyline` | boolean | false | Emits the `ee.status.skyline.v1` status block in JSON and compact human output; full community scoring remains owned by G8.a composite skyline work. |

Example:

```bash
ee insights --section proximityHotspots --workspace . --limit 5 --json
ee proximity mem_release_policy mem_rch_remote_required --workspace . --json
ee why mem_failed_release --causal-explain --workspace . --json
ee status --skyline --workspace . --json
```

## Graph Command Flags

The read-only graph algorithms `pagerank`, `betweenness`, `hits`,
`communities`, and `articulation` share this filter set:

| Flag | Values | Default | Use |
| --- | --- | --- | --- |
| `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads graph source tables. |
| `--min-weight <WEIGHT>` | `0.0..=1.0` | omitted | Excludes low-weight memory links. |
| `--min-confidence <CONFIDENCE>` | `0.0..=1.0` | omitted | Excludes low-confidence memory links. |
| `--link-limit <COUNT>` | integer | omitted | Caps links processed. |
| `--limit <COUNT>` | integer | omitted | Caps emitted rows, nodes, or communities. |
| `--include-tombstoned` | boolean | false | Includes tombstoned memory nodes. |

Command-specific graph flags:

| Command | Flag | Values | Default | Use |
| --- | --- | --- | --- | --- |
| `ee graph louvain` | shared `--database`, `--min-weight`, `--min-confidence`, `--link-limit`, `--limit` | see above | see above | Filters the undirected memory-link graph. |
| `ee graph louvain` | `--resolution <FLOAT>` | float | `1.0` | Sets Louvain modularity resolution. |
| `ee graph louvain` | `--threshold <FLOAT>` | float | `1.0e-7` | Sets the Louvain improvement threshold. |
| `ee graph louvain` | `--max-level <COUNT>` | integer | omitted | Stops after a bounded number of Louvain levels. |
| `ee graph louvain` | `--seed <SEED>` | integer | omitted | Selects deterministic Louvain seed. |
| `ee graph k-core` | `--k <K>` | integer | main core | Extracts a specific core number. |
| `ee graph path <SRC> <DST>` | `--database`, `--min-weight`, `--min-confidence`, `--link-limit` | see above | see above | Finds a shortest path between two memories. |
| `ee graph explain-link <SRC> <DST>` | `--database`, `--min-weight`, `--min-confidence`, `--link-limit` | see above | see above | Explains direct and path-based evidence between two memories. |
| `ee graph export` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads the snapshot registry. |
| `ee graph export` | `--workspace-id <ID>` | workspace ID | current workspace stable ID | Selects the workspace snapshot namespace. |
| `ee graph export` | `--snapshot-id <ID>` | snapshot ID | latest by type | Exports a specific graph snapshot. |
| `ee graph export` | `--graph-type <TYPE>` / `--type <TYPE>` | graph snapshot type | `memory_links` | Selects the graph family to export. |
| `ee graph centrality` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads persisted centrality scores from the latest memory-link snapshot. |
| `ee graph centrality` | `--algorithm <ALGORITHM>` | `pagerank`, `betweenness`, `authority`, `hits-hubs`, `hits-authorities` | `pagerank` | Selects which persisted centrality score family to list. |
| `ee graph centrality` | `--limit <COUNT>` | integer | `10` | Caps returned centrality rows. |
| `ee graph centrality` | `--memory-id <MEMORY_ID>` | memory ID | omitted | Returns scores for one memory instead of top rows. |
| `ee graph centrality` | `--require-fresh` | boolean | false | Exits 6 when the latest graph snapshot is stale. |
| `ee graph centrality-refresh` | `--dry-run` | boolean | false | Reports the refresh plan without computing. |
| `ee graph centrality-refresh` | `--database`, `--min-weight`, `--min-confidence`, `--link-limit` | see above | see above | Filters the centrality refresh graph. |
| `ee graph snapshot refresh` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads and writes graph snapshots. |
| `ee graph snapshot refresh` | `--dry-run` | boolean | false | Reports the refresh plan without persisting. |
| `ee graph snapshot refresh` | `--graph <GRAPH>` | `memory_links`, `causal`, `revision`, `rules`, `contradictions`, `all` | `memory_links` | Selects which graph family to refresh. |
| `ee graph snapshot refresh` | `--min-weight`, `--min-confidence`, `--link-limit` | see above | omitted | Filters `memory_links` refreshes. |
| `ee graph feature-enrichment` | `--dry-run` | boolean | false | Computes only the projection plan; enriched features are degraded. |
| `ee graph feature-enrichment` | `--database`, `--min-weight`, `--min-confidence`, `--link-limit` | see above | see above | Filters the enrichment graph. |
| `ee graph feature-enrichment` | `--max-features <COUNT>` | integer | omitted | Caps emitted enriched features. |
| `ee graph feature-enrichment` | `--min-combined-score <SCORE>` | `0.0..=1.0` | omitted | Drops graph features below a combined score threshold. |
| `ee graph feature-enrichment` | `--max-selection-boost <BOOST>` | float | omitted | Caps derived selection boosts. |
| `ee graph neighborhood <MEMORY_ID>` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads memory links. |
| `ee graph neighborhood <MEMORY_ID>` | `--direction <DIRECTION>` | `incoming`, `outgoing`, `both` | `both` | Filters incident edges by direction. |
| `ee graph neighborhood <MEMORY_ID>` | `--relation <RELATION>` | relation name | omitted | Restricts edges to one memory-link relation. |
| `ee graph neighborhood <MEMORY_ID>` | `--limit <COUNT>` | integer | omitted | Caps edges after deterministic ordering. |

Example:

```bash
ee graph snapshot refresh --workspace . --graph memory_links --dry-run --json
ee graph centrality --workspace . --algorithm pagerank --limit 10 --json
```

## Curation And Maintenance Flags

| Command | Flag | Values | Default | Use |
| --- | --- | --- | --- | --- |
| `ee curate disposition` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads curation candidates and memory state. |
| `ee curate disposition` | `--actor <ACTOR>` | string | omitted | Records the actor when `--apply` writes audit metadata. |
| `ee curate disposition` | `--apply` | boolean | false | Applies deterministic TTL transitions. Omit for dry-run planning. |
| `ee curate disposition` | `--no-structural-decay` | boolean | false | Uses legacy uniform TTL disposition without graph structural adjustments. |
| `ee curate disposition` | `--now <RFC3339>` | timestamp | current time | Overrides the current time for deterministic replay. |
| `ee maintenance run` | `--job <JOB>` | steward job kind | `decay_sweep` | Selects the maintenance job. |
| `ee maintenance run` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads and writes maintenance state. |
| `ee maintenance run` | `--dry-run` | boolean | false | Reports planned work without mutating memory scores. |
| `ee maintenance run` | `--include-decay` | boolean | false | Includes L3 decay lifecycle actions. |
| `ee maintenance run` | `--no-structural-decay` | boolean | false | Uses legacy uniform decay without graph structural adjustments. |
| `ee maintenance run` | `--as-of <RFC3339>` | timestamp | current time | Replays maintenance against a deterministic reference time. |
| `ee maintenance run` | `--time-limit-ms <MS>` | integer | job default | Overrides per-job time budget. |
| `ee maintenance run` | `--item-limit <N>` | integer | job default | Overrides per-job item budget. |
| `ee maintenance graph-snapshot-prune` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads graph snapshot rows. |
| `ee maintenance graph-snapshot-prune` | `--dry-run` | boolean | false | Reports planned pruning without mutating graph snapshot rows. |
| `ee maintenance graph-snapshot-prune` | `--time-limit-ms <MS>` | integer | job default | Overrides per-job time budget. |
| `ee maintenance graph-snapshot-prune` | `--item-limit <N>` | integer | job default | Overrides per-job item budget. |
| `ee maintenance graph-witnesses-prune` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads graph algorithm witness rows. |
| `ee maintenance graph-witnesses-prune` | `--dry-run` | boolean | false | Reports planned witness pruning without mutating witness rows. |
| `ee maintenance graph-witnesses-prune` | `--retention-days <DAYS>` | integer days | witness policy default | Overrides the default witness retention window. |
| `ee maintenance graph-witnesses-prune` | `--algorithm-ttl <NAME=DAYS>` | repeatable key/value | omitted | Overrides one algorithm-specific witness TTL; repeat for multiple algorithms. |
| `ee job run <KIND>` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Runs a steward handler against an explicit DB. |
| `ee job run <KIND>` | `--dry-run` | boolean | false | Reports planned work without mutating memory scores or job history. |
| `ee job run <KIND>` | `--time-limit-ms <MS>` | integer | job default | Overrides per-job time budget. |
| `ee job run <KIND>` | `--item-limit <N>` | integer | job default | Overrides per-job item budget. |

Example:

```bash
ee maintenance run --workspace . --job decay_sweep \
  --no-structural-decay --dry-run --json
```

## Tracked But Not Yet In Current CLI

These names appear in the GraphAccretion/docs roadmap, but the current Clap
surface in this checkout does not yet expose them as top-level flags:

| Planned flag | Tracked surface | Current status |
| --- | --- | --- |
| `--allow-tombstone-load-bearing` | load-bearing curation policy | Not present in the current Clap structs. Use `--no-structural-decay` for the implemented opt-out surface. |
