# Graph CLI Flags

This reference aggregates the graph-related flags that agents are most likely
to combine across context packing, insights, maintenance, and graph inspection.
It is derived from the current Clap surfaces in `src/cli/mod.rs` and
`src/cli/insights/mod.rs`.

For precedence rules when these flags are combined, see
[`flag-precedence.md`](./flag-precedence.md).

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

Example:

```bash
ee --fields standard --cards summary --meta graph pagerank \
  --workspace . --limit 5 --json
```

## Context And Pack Flags

`ee context "<task>"` is the main graph-aware retrieval surface. `ee pack` and
`ee pack build` share most pack assembly flags, except `ee context` currently
owns `--ppr-weight`, `--explain`, and `--no-pack-dna`.

| Command | Flag | Values | Default | Use |
| --- | --- | --- | --- | --- |
| `ee context` | `--max-tokens`, `-t` | integer | `4000` | Sets the context pack token budget. |
| `ee context`, `ee pack`, `ee pack build` | `--candidate-pool` | integer | `100` for `context`; query-file/default for `pack` | Caps candidates retrieved before packing. |
| `ee context`, `ee pack`, `ee pack build` | `--speed` | `instant`, `default`, `quality` | `default` for `context`; query-file/default for `pack` | Selects retrieval speed versus quality budget. |
| `ee context`, `ee pack`, `ee pack build` | `--profile`, `-p` | `compact`, `balanced`, `grounding`, `orientation`, `thorough`, `submodular` | `balanced` for `context`; query-file/default for `pack` | Selects the context profile, section quota strategy, and HITS profile boosts when available. |
| `ee context` | `--ppr-weight <WEIGHT>` | float; clamped to `0.0..=1.0` | omitted | Blends Personalized PageRank graph pull into context ranking. |
| `ee context`, `ee pack`, `ee pack build` | `--pack-profile <PROFILE>` | `lean`, `standard`, `verbose` | `standard` for `context`; query-file/default for `pack` | Controls optional pack metadata volume. |
| `ee context`, `ee pack`, `ee pack build` | `--resource-profile <PROFILE>` | `lean`, `standard`, `swarm_heavy` | `standard` for `context`; query-file/default for `pack` | Selects pack assembly SLOs and resource assumptions. |
| `ee context`, `ee pack`, `ee pack build` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads the source-of-truth memory and graph-link tables. |
| `ee context`, `ee pack`, `ee pack build` | `--index-dir <PATH>` | filesystem path | `<workspace>/.ee/index/` | Reads derived search indexes before graph-aware packing. |
| `ee context`, `ee pack`, `ee pack build`, `ee search` | `--explain-performance` | boolean | false | Emits a redaction-safe query or pack performance report instead of normal hits or pack output. |
| `ee context` | `--explain` | boolean | false | Adds graph-derived Pack DNA metadata to JSON output. |
| `ee context` | `--no-pack-dna` | boolean | false | Suppresses `data.pack.packDna` even when `--explain` is set. |
| `ee context` | `--stream` | boolean | false | Emits `ee.pack.stream.v1` NDJSON frames; requires `--json`, `--robot`, `--format json`, or `--format jsonl` and cannot be combined with `--explain-performance`. |
| `ee context` | `--include-tombstoned` | boolean | false | Includes tombstoned memories in context results and graph-aware ranking, with lifecycle metadata. Without this flag, tombstoned nodes are pruned before PPR and Pack DNA neighbor selection. |
| `ee context` | `--changed-symbol <SYMBOL>` | repeatable string | omitted | Boosts memories linked to a changed Rust symbol selector. See [`symbol-graph.md`](../agent-ux/symbol-graph.md) for the current Rust-first contract and degraded states. |
| `ee context` | `--changed-symbols-from-git` | boolean | false | Derives changed Rust symbol selectors from the current git diff and applies the same bounded symbol boost. |
| `ee context`, `ee pack`, `ee pack build` | `--no-coverage-fill[=BOOL]` | optional boolean | false | Disables the coverage-fill pass; pass `--no-coverage-fill=false` to override a lean profile. |
| `ee context`, `ee pack`, `ee pack build` | `--no-rendered-text[=BOOL]` | optional boolean | false | Suppresses rendered pack text in JSON output. |
| `ee context`, `ee pack`, `ee pack build` | `--no-skipped[=BOOL]` | optional boolean | false | Suppresses omitted/skipped item explanations. |
| `ee context`, `ee pack`, `ee pack build` | `--no-meta[=BOOL]` | optional boolean | false | Suppresses pack metadata. |
| `ee pack`, `ee pack build` | `--coordination-snapshot <PATH>` | JSON file path | omitted | Embeds a redacted coordination snapshot in the pack. |
| `ee pack`, `ee pack build` | `--coordination-stale-after-ms <MS>` | integer milliseconds | package default | Marks coordination sources stale after the configured age. |
| `ee pack`, `ee pack build` | `--include-non-affecting-degradations[=BOOL]` | optional boolean | false | Keeps non-affecting degraded signals in `data.degraded[]`. |
| `ee pack`, `ee pack build` | `--as-of <RFC3339>` | timestamp | now | Replays validity-window filtering at a deterministic time. |
| `ee pack`, `ee pack build` | `--include-expired` | boolean | false | Includes memories whose `valid_to` is before the reference time. |
| `ee pack`, `ee pack build` | `--include-future` | boolean | false | Includes memories whose `valid_from` is after the reference time. |
| `ee pack`, `ee pack build` | `--include-stale` | boolean | false | Includes memories marked with stale validity status in index metadata. |
| `ee pack replay <PACK_ID>` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Replays a persisted pack selection ledger without rebuilding or reselecting context. |
| `ee pack diff <PACK_A> <PACK_B>` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Compares two persisted pack ledgers and explains selection, freshness, or redaction drift. |

Example:

```bash
ee context "prepare release" --workspace . --profile thorough \
  --ppr-weight 0.5 --explain --json
ee context "ground release evidence" --workspace . --profile grounding --json
ee context "map release dependencies" --workspace . --profile orientation --json
ee context "prepare release" --workspace . --stream --format json
ee context "prepare release" --workspace . --explain-performance --json
ee context "prepare release" --workspace . --ppr-weight 0.4 \
  --include-tombstoned --explain --json
ee context "review changed context scoring" --workspace . \
  --changed-symbol apply_changed_symbol_context_boost --json
ee context "review current Rust edits" --workspace . \
  --changed-symbols-from-git --json
ee pack build --workspace . --query-file release.eeq.json \
  --candidate-pool 150 --speed quality --profile thorough \
  --pack-profile verbose --resource-profile swarm_heavy \
  --explain-performance \
  --coordination-snapshot coordination.json \
  --include-non-affecting-degradations --as-of 2026-05-19T00:00:00Z \
  --include-stale --json
ee pack replay pack_release_prev --workspace . --database .ee/ee.db --json
ee pack diff pack_release_prev pack_release_next --workspace . --json
ee search "release blockers" --workspace . --explain-performance --json
```

## Insights And Narrow Graph Questions

| Command | Flag | Values | Default | Use |
| --- | --- | --- | --- | --- |
| `ee insights` | `--section <NAME>` | section name | omitted | Emits one deterministic insights section, such as `bridges`, `proximityHotspots`, `knowledgeSkyline`, `hubs`, `authorities`, or `loadBearingMemories`. |
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
| `ee rule provenance <RULE_ID>` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads the rule-to-memory provenance ego graph. |
| `ee health` | `--robot-insights` | boolean | false | Emits graph-derived structural health for robot consumers. |
| `ee status` | `--skyline` | boolean | false | Emits the `ee.status.skyline.v1` status block in JSON and compact human output; full community scoring remains owned by G8.a composite skyline work. |

Example:

```bash
ee insights --section proximityHotspots --workspace . --limit 5 --json
ee insights --section hubs --workspace . --limit 5 --json
ee insights --section authorities --workspace . --limit 5 --json
ee insights --section loadBearingMemories --workspace . --limit 5 --json
ee insights --explain mem_failed_release --workspace . --limit 5 --json
ee proximity mem_release_policy mem_rch_remote_required --workspace . --json
ee proximity mem_release_policy mem_rch_remote_required --workspace . \
  --min-weight 0.4 --min-confidence 0.6 --link-limit 250 \
  --include-tombstoned --json
ee why mem_failed_release --causal-explain --confidence-threshold 0.7 \
  --workspace . --json
ee rule provenance rule_release_policy --workspace . --json
ee health --robot-insights --workspace . --json
ee status --skyline --workspace . --json
```

`hubs` and `authorities` require `graph.feature.hits_profiles.enabled`.
`loadBearingMemories` requires `graph.feature.load_bearing.enabled`. When a
section is disabled, `ee insights --section ... --json` returns a degraded
signal with the repair command, such as
`ee config set graph.feature.hits_profiles.enabled true` or
`ee config set graph.feature.load_bearing.enabled true`.

## Causal Command Flags

`ee causal` is the graph-derived explanation surface for recorder, pack,
preflight, tripwire, procedure, experiment, and outcome evidence. Use `--dry-run`
while wiring automation so the command reports the plan without querying or
promoting live evidence.

| Command | Flag | Values | Default | Use |
| --- | --- | --- | --- | --- |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--memory-id <MEMORY_ID>` | memory ID | omitted | Filters causal chains to one memory when no positional failure ID is enough. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--run-id <RUN_ID>` | recorder run ID | omitted | Filters evidence by recorder run. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--pack-id <PACK_ID>` | context pack ID | omitted | Filters evidence by context pack. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--preflight-id <PREFLIGHT_ID>` | preflight ID | omitted | Filters evidence by preflight decision. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--tripwire-id <TRIPWIRE_ID>` | tripwire ID | omitted | Filters evidence by tripwire firing. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--procedure-id <PROCEDURE_ID>` | procedure ID | omitted | Filters evidence by promoted procedure. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--agent-id <AGENT_ID>` | agent ID | omitted | Filters evidence by agent identity. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads causal evidence from an explicit database for diagnostic replay. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--database-workspace-id <WORKSPACE_ID>` | workspace ID | current workspace stable ID | Selects the workspace ID stored in the explicit causal database. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--limit`, `-n` | integer | `50` | Caps causal chains returned. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--depth <N>` | integer | `8` | Caps backward causal edge traversal. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--include-exposures` | boolean | false | Includes detailed exposure rows in output. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--include-outcomes` | boolean | false | Includes outcome summaries in output. |
| `ee causal trace [FAILURE_MEMORY_ID]` | `--dry-run` | boolean | false | Reports the trace query plan without executing it. |
| `ee causal compare [CHAIN_A] [CHAIN_B]` | `--fixture-replay-id <ID>` | fixture replay ID | omitted | Includes replay evidence in the comparison. |
| `ee causal compare [CHAIN_A] [CHAIN_B]` | `--shadow-run-id <ID>` | shadow-run ID | omitted | Includes shadow-run evidence in the comparison. |
| `ee causal compare [CHAIN_A] [CHAIN_B]` | `--counterfactual-episode-id <ID>` | counterfactual episode ID | omitted | Includes counterfactual evidence in the comparison. |
| `ee causal compare [CHAIN_A] [CHAIN_B]` | `--experiment-id <ID>` | experiment ID | omitted | Includes active-learning experiment evidence. |
| `ee causal compare [CHAIN_A] [CHAIN_B]`, `ee causal estimate`, `ee causal promote-plan` | `--artifact-id <ARTIFACT_ID>` | artifact ID | omitted | Scopes causal evidence to one artifact. |
| `ee causal compare [CHAIN_A] [CHAIN_B]`, `ee causal estimate`, `ee causal promote-plan` | `--decision-id <DECISION_ID>` | decision ID | omitted | Scopes causal evidence to one decision. |
| `ee causal compare [CHAIN_A] [CHAIN_B]`, `ee causal estimate`, `ee causal promote-plan` | `--method <METHOD>` | `naive`, `matching`, `replay`, `experiment` | `replay` for compare/promote; `naive` for estimate | Selects the causal comparison or estimation method. |
| `ee causal compare [CHAIN_A] [CHAIN_B]` | `--dry-run` | boolean | false | Reports comparison inputs without generating concrete comparisons. |
| `ee causal estimate [CHAIN_ID]` | `--chain-id <CHAIN_ID>` | causal chain ID | omitted | Selects the chain when the positional argument is absent or generated upstream. |
| `ee causal estimate [CHAIN_ID]` | `--agent-id <AGENT_ID>` | agent ID | omitted | Filters estimates by agent. |
| `ee causal estimate [CHAIN_ID]` | `--include-confounders` | boolean | false | Includes identified confounders in output. |
| `ee causal estimate [CHAIN_ID]` | `--include-assumptions` | boolean | false | Includes assumptions used during estimation. |
| `ee causal estimate [CHAIN_ID]` | `--dry-run` | boolean | false | Reports the estimation plan without computing. |
| `ee causal promote-plan [CHAIN_ID]` | `--estimate-id <ESTIMATE_ID>` | estimate ID | omitted | Scopes promotion planning to one estimate. |
| `ee causal promote-plan [CHAIN_ID]` | `--action <ACTION>` | `promote`, `hold`, `demote`, `archive`, `quarantine` | inferred | Requests an explicit target posture action. |
| `ee causal promote-plan [CHAIN_ID]` | `--minimum-uplift <UPLIFT>` | float | `0.05` | Requires a minimum estimated uplift before promotion. |
| `ee causal promote-plan [CHAIN_ID]` | `--include-revalidation` | boolean | false | Includes explicit revalidation recommendations. |
| `ee causal promote-plan [CHAIN_ID]` | `--include-narrower-routing` | boolean | false | Includes narrower routing recommendations. |
| `ee causal promote-plan [CHAIN_ID]` | `--include-experiment-proposals` | boolean | false | Includes experiment proposals. |
| `ee causal promote-plan [CHAIN_ID]` | `--dry-run` | boolean | false | Keeps the command in planning mode for automation. |

Example:

```bash
ee causal trace mem_failed_release --workspace . --depth 3 \
  --include-exposures --include-outcomes --dry-run --json
ee causal compare chain_baseline chain_candidate --workspace . \
  --fixture-replay-id fixture_release --method replay --dry-run --json
ee causal estimate chain_release --workspace . --method matching \
  --include-confounders --include-assumptions --dry-run --json
ee causal promote-plan chain_release --workspace . --action promote \
  --minimum-uplift 0.08 --include-revalidation --dry-run --json
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
| `ee graph k-core` | `--database`, `--min-weight`, `--min-confidence`, `--link-limit` | see above | see above | Filters the undirected memory-link graph before extracting cores. |
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
ee graph pagerank --workspace . --min-weight 0.2 --min-confidence 0.5 \
  --link-limit 500 --limit 10 --include-tombstoned --json
ee graph betweenness --workspace . --min-weight 0.3 --limit 10 --json
ee graph hits --workspace . --min-confidence 0.6 --limit 10 --json
ee graph communities --workspace . --link-limit 500 --limit 5 --json
ee graph articulation --workspace . --include-tombstoned --limit 10 --json
ee graph louvain --workspace . --resolution 1.2 --threshold 0.000001 \
  --max-level 4 --seed 42 --limit 5 --json
ee graph export --workspace . --graph-type memory_links \
  --workspace-id ws_release --snapshot-id snap_release --format mermaid
ee graph export --workspace . --type memory_links \
  --workspace-id ws_release --snapshot-id snap_release --format mermaid
ee graph centrality --workspace . --algorithm pagerank --limit 10 --json
ee graph centrality --workspace . --algorithm hits-hubs --limit 10 --json
ee graph centrality --workspace . --algorithm hits-authorities \
  --memory-id mem_release_policy --require-fresh --json
ee graph centrality-refresh --workspace . --dry-run --min-confidence 0.6 --json
ee graph k-core --workspace . --k 3 --min-confidence 0.6 --json
ee graph path mem_source mem_target --workspace . --min-weight 0.4 --json
ee graph explain-link mem_source mem_target --workspace . --link-limit 250 --json
ee graph feature-enrichment --workspace . --dry-run --max-features 25 \
  --min-combined-score 0.2 --max-selection-boost 0.4 --json
ee graph neighborhood mem_release_policy --workspace . --direction incoming \
  --relation supports --limit 20 --json
```

## Backup And Restore Graph-Cache Flags

Backups include graph-derived cache artifacts by default so restored workspaces
can keep graph snapshots, algorithm witnesses, and result-cache rows warm when
the source manifest proves them. Use the explicit graph-cache flags when you
need a source-only backup or a cold-cache restore.

| Command | Flag | Values | Default | Use |
| --- | --- | --- | --- | --- |
| `ee backup create` | `--include-graph-cache[=BOOL]` | optional boolean | `true` | Includes graph snapshot, witness, and result-cache assets in the backup manifest. Pass `--include-graph-cache=false` for a source-only archive. |
| `ee backup restore <BACKUP_ID_OR_PATH>` | `--skip-graph-cache` | boolean | false | Restores durable records while leaving graph-cache assets cold for later rebuild. |

Example:

```bash
ee backup create --workspace . --label pre-refactor \
  --include-graph-cache=false --dry-run --json
ee backup restore bk_release --workspace . --side-path ./restore-check \
  --skip-graph-cache --dry-run --json
```

## Diagnostic Graph Fixtures

`ee diag graph` is the read-only graph module readiness check. The fixture
commands seed deterministic graph-related diagnostic rows for contract and
failure-mode replay. They are fixture surfaces, not ordinary user workflows, and
should be run against disposable or explicitly selected databases.

| Command | Flag | Values | Default | Use |
| --- | --- | --- | --- | --- |
| `ee diag graph` | none | none | none | Reports graph module readiness, capabilities, and metrics without seeding fixtures. |
| `ee diag causal-edge` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Writes the fixture causal edge to an explicit diagnostic database. |
| `ee diag causal-edge` | `--workspace-id <WORKSPACE_ID>` | workspace ID | current workspace stable ID | Stores the fixture edge under a deterministic workspace namespace. |
| `ee diag causal-edge` | `--edge-id <EDGE_ID>` | edge ID | required | Sets the causal evidence edge ID. |
| `ee diag causal-edge` | `--failure-id <MEMORY_ID>` | memory ID | required | Selects the failure memory endpoint. |
| `ee diag causal-edge` | `--candidate-cause-id <MEMORY_ID>` | memory ID | required | Selects the candidate cause memory endpoint. |
| `ee diag causal-edge` | `--contribution-score <SCORE>` | `0.0..=1.0` | `0.7` | Records the causal contribution score. |
| `ee diag causal-edge` | `--evidence-uri <URI>` | URI, repeatable | omitted | Preserves provenance URIs on the edge. |
| `ee diag causal-edge` | `--computed-at <RFC3339>` | timestamp | current time | Replays the diagnostic edge with a deterministic timestamp. |
| `ee diag causal-edge` | `--method <METHOD>` | `manual`, `graph-inferred`, `cass-derived` | `manual` | Labels the causal evidence method. |
| `ee diag graph-snapshot` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Writes the fixture graph snapshot row to an explicit database. |
| `ee diag graph-snapshot` | `--status <STATUS>` | `valid`, `stale`, `invalid`, `archived` | `valid` | Seeds the snapshot lifecycle status. |
| `ee diag graph-snapshot` | `--metrics-json <JSON>` | JSON object | omitted | Stores deterministic graph metrics with the snapshot row. |
| `ee diag graph-snapshot` | `--node-count <N>` | integer | `1` | Records fixture node count metadata. |
| `ee diag graph-snapshot` | `--edge-count <N>` | integer | `0` | Records fixture edge count metadata. |
| `ee diag graph-snapshot` | `--source-generation <N>` | integer | `1` | Records fixture source-generation metadata. |

Example:

```bash
ee diag graph --workspace . --json
ee diag causal-edge --workspace . --edge-id edge_release_failure \
  --failure-id mem_failed_release --candidate-cause-id mem_missing_rch_proof \
  --contribution-score 0.8 --evidence-uri file://proof.json --json
ee diag graph-snapshot --workspace . --status stale \
  --metrics-json '{"pagerank":1}' --node-count 42 --edge-count 77 --json
```

## Swarm And Host-Readiness Flags

These commands are read-only readiness surfaces for agent swarms and host
configuration. They are included here because graph-heavy work often depends on
fresh coordination, host-capacity, and profile evidence before an agent picks a
bead or runs an RCH-gated verification.

| Command | Flag | Values | Default | Use |
| --- | --- | --- | --- | --- |
| `ee swarm brief` | `--sources <LIST>` | comma-separated: `default`, `all`, `none`, `git`, `beads`, `bv`, `agent-mail`, `rch`, `host-profile`, `agent-inventory` | `default` | Selects read-only inputs for the coordination brief. |
| `ee swarm next-action` | `--sources <LIST>` | same as `ee swarm brief` | `default`; includes RCH for next-action | Selects read-only inputs for work allocation. |
| `ee swarm brief`, `ee swarm next-action` | `--include-rch` | boolean | false | Adds the optional RCH status probe; equivalent to including `rch` in `--sources`. |
| `ee swarm brief`, `ee swarm next-action` | `--agent-mail-snapshot <PATH>` | JSON file path | omitted | Includes a redacted Agent Mail snapshot without mutating live mail. |
| `ee swarm next-action` | `--verifier-evidence <PATH>` | `ee.rch.verify.v1` proof JSON path | omitted | Includes recent compile-health evidence for work-allocation preflight. |
| `ee swarm brief`, `ee swarm next-action` | `--agent-inventory-only <SLUGS>` | comma-separated connector slugs | omitted | Limits agent inventory inspection to selected connectors when inventory is enabled. |
| `ee swarm brief`, `ee swarm next-action` | `--max-recent-commits <N>` | integer | `8` | Caps recent git commits included by the git source. |
| `ee swarm brief`, `ee swarm next-action` | `--command-timeout-ms <MS>` | integer milliseconds | `1500` | Sets the timeout budget for each selected source probe. |
| `ee swarm brief`, `ee swarm next-action` | `--require-sources` | boolean | false | Exits 6 when any selected source is unavailable, unconfigured, or skipped. |
| `ee diag host-profile` | `--full-paths` | boolean | false | Includes absolute host paths in path probes; omit for redacted labels. |
| `ee profile config plan` | `--profile <PROFILE>` | `constrained`, `portable`, `workstation`, `swarm` | host-adaptive recommendation | Plans exact `.ee/config.toml` profile changes without writing. |
| `ee profile config plan`, `ee profile config apply` | `--config <PATH>` | filesystem path | `<workspace>/.ee/config.toml` | Selects the config file to inspect or update. |
| `ee profile config apply` | `--profile <PROFILE>` | `constrained`, `portable`, `workstation`, `swarm` | host-adaptive recommendation | Selects the requested operating profile for the config write. |
| `ee profile config apply` | `--dry-run` | boolean | false | Reports the planned write without mutating the config file. |

Example:

```bash
ee swarm brief --workspace . \
  --sources git,beads,bv,agent-mail --require-sources --json
ee swarm brief --workspace . --sources host-profile,agent-inventory \
  --agent-inventory-only codex,claude --max-recent-commits 4 \
  --command-timeout-ms 750 --json
ee swarm next-action --workspace . --sources default,host-profile \
  --verifier-evidence proof.json --include-rch --json
ee diag host-profile --workspace . --full-paths --json
ee profile config plan --workspace . --profile swarm \
  --config .ee/config.toml --json
ee profile config apply --workspace . --profile portable \
  --config .ee/config.toml --dry-run --json
```

## Curation And Maintenance Flags

| Command | Flag | Values | Default | Use |
| --- | --- | --- | --- | --- |
| `ee curate disposition` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads curation candidates and memory state. |
| `ee curate disposition` | `--actor <ACTOR>` | string | omitted | Records the actor when `--apply` writes audit metadata. |
| `ee curate disposition` | `--apply` | boolean | false | Applies deterministic TTL transitions. Omit for dry-run planning. |
| `ee curate disposition` | `--no-structural-decay` | boolean | false | Uses legacy uniform TTL disposition without graph structural adjustments. |
| `ee curate disposition` | `--now <RFC3339>` | timestamp | current time | Overrides the current time for deterministic replay. |
| `ee curate apply <CANDIDATE_ID>` | `--allow-tombstone-load-bearing` | boolean | false | Permits applying a tombstone or retraction candidate after reviewing load-bearing graph evidence. |
| `ee curate tombstone <MEMORY_ID>` | `--allow-tombstone-load-bearing` | boolean | false | Permits an explicit tombstone after reviewing the memory's load-bearing why graph badge. |
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
| `ee maintenance wal-checkpoint` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Reads WAL status and runs the checkpoint through the explicit writer path. |
| `ee maintenance wal-checkpoint` | `--mode <MODE>` | `passive`, `truncate` | `passive` | Selects the SQLite checkpoint mode. |
| `ee maintenance wal-checkpoint` | `--dry-run` | boolean | false | Reports WAL status without running a checkpoint. |
| `ee maintenance status` | none | none | none | Reports maintenance job availability and the next suggested steward command. |
| `ee job run <KIND>` | `--database <PATH>` | filesystem path | `<workspace>/.ee/ee.db` | Runs a steward handler against an explicit DB. |
| `ee job run <KIND>` | `--dry-run` | boolean | false | Reports planned work without mutating memory scores or job history. |
| `ee job run <KIND>` | `--time-limit-ms <MS>` | integer | job default | Overrides per-job time budget. |
| `ee job run <KIND>` | `--item-limit <N>` | integer | job default | Overrides per-job item budget. |
| `ee job list` | `--kind <KIND>` | steward job kind | omitted | Filters durable job history rows to one maintenance job family. |
| `ee job list` | `--since <RFC3339>` | timestamp | omitted | Filters durable job history rows to records at or after the timestamp. |
| `ee job list` | `--limit`, `-n` | integer | `20` | Caps durable job history rows returned. |
| `ee job show <JOB_ID>` | `JOB_ID` | job history row ID | required | Shows one durable job history row, or the latest match for a steward runner job ID. |

Example:

```bash
ee maintenance run --workspace . --job decay_sweep \
  --no-structural-decay --dry-run --json
ee curate disposition --workspace . --no-structural-decay \
  --now 2026-05-19T00:00:00Z --json
ee curate tombstone mem_load_bearing_rule --workspace . \
  --allow-tombstone-load-bearing --dry-run --json
ee curate apply cand_retract_stale_rule --workspace . \
  --allow-tombstone-load-bearing --dry-run --json
ee job run centrality_refresh --workspace . --dry-run \
  --time-limit-ms 500 --item-limit 25 --json
ee maintenance graph-snapshot-prune --workspace . --dry-run \
  --time-limit-ms 500 --item-limit 25 --json
ee maintenance graph-witnesses-prune --workspace . --dry-run \
  --retention-days 30 --algorithm-ttl pagerank=14 --json
ee maintenance wal-checkpoint --workspace . --mode truncate --dry-run --json
ee maintenance status --workspace . --json
ee job list --workspace . --kind centrality_refresh \
  --since 2026-05-19T00:00:00Z --limit 10 --json
ee job show job_release --workspace . --json
```

## Tracked But Not Yet In Current CLI

These names appear in the GraphAccretion/docs roadmap, but the current Clap
surface in this checkout does not yet expose them as top-level flags:

| Planned flag | Tracked surface | Current status |
| --- | --- | --- |
| `--causal-explain` on non-`why` commands | causal graph expansion | Only `ee why <MEMORY_ID> --causal-explain` exposes the current causal explanation flag. |
