# CLI Flag Precedence

This reference documents how to read `ee` commands when graph, pack, renderer,
and curation flags are combined. It is scoped to current Clap surfaces in
`src/cli/mod.rs`; planned flags stay in the "not current" section until the
parser exposes them.

## Summary

| Rule | Precedence |
| --- | --- |
| Renderer selection | `--json` and `--robot` force JSON for otherwise human-oriented output; explicit `--format` selects the renderer for the whole response. |
| Explicit disables | Implemented `--no-*` flags suppress their target feature or field even when an enabling flag or profile is also present. |
| Boolean override disables | Flags with `[=BOOL]` support `--no-X=false` to override a profile default that would otherwise suppress `X`. |
| Profiles and weights | `--profile` selects pack or retrieval policy; explicit numeric weights such as `--ppr-weight` compose with the profile instead of replacing it. |
| Graph freshness | `--require-fresh` changes stale persisted graph centrality from usable-with-metadata to an exit-6 requirement failure. |
| Maintenance mutation | `--dry-run` keeps maintenance and curation planning read-only; `--apply` or a mutating maintenance command is required for durable changes. |
| Tombstone lifecycle | Explicit tombstone commands are audited lifecycle mutations; load-bearing overrides apply only when `--allow-tombstone-load-bearing` is explicitly present. |

## Renderer And Envelope

Renderer selection is global. A command does not render one field as JSON and
another field as TOON or Markdown; `--format` chooses the response renderer, and
field-level shaping is handled by field presets or command-specific omit flags.

Worked examples:

```bash
ee context "prepare release" --workspace . --format json
ee context "prepare release" --workspace . --json
ee graph export --workspace . --format mermaid
ee graph export --workspace . --format mermaid --json
```

Precedence:

- `--json` and `--robot` select JSON when the format would otherwise be human.
- `--format mermaid` uses the Mermaid/Markdown-oriented path when the command
  supports it; adding `--json` or `--robot` requests the JSON response instead.
- `--schema-version` affects the response envelope version, not the command's
  internal business logic.
- `--fields` changes which JSON fields are emitted; it does not re-run the
  command with different graph or pack semantics.

## Context And Pack

`ee context` owns graph-aware ranking flags such as `--ppr-weight`, while
`ee context`, `ee pack`, and `ee pack build` share most pack-output shaping
flags.

Worked examples:

```bash
ee context "prepare release" --workspace . \
  --profile balanced --ppr-weight 0 --explain --no-pack-dna --json

ee context "prepare release" --workspace . \
  --profile thorough --ppr-weight 0.7 --explain --json

ee context "ground release evidence" --workspace . \
  --profile grounding --ppr-weight 0.4 --json

ee context "map release dependencies" --workspace . \
  --profile orientation --json

ee pack build --workspace . --query-file release.eeq.json \
  --profile compact --no-coverage-fill=false \
  --no-rendered-text --no-skipped --json
```

Precedence:

- `--ppr-weight 0` disables the Personalized PageRank contribution while still
  leaving the selected `--profile` active for other pack and retrieval choices.
- `--ppr-weight 0.7` composes with `--profile`; it does not replace profile
  behavior such as candidate budgets or section strategy.
- `--profile grounding` and `--profile orientation` keep balanced section
  quotas while selecting the HITS authority or hub boost policy. `balanced`
  applies both HITS axes at half strength.
- `--explain --no-pack-dna` means "run context with explain mode, but omit
  `data.pack.packDna` from the emitted JSON."
- `--no-coverage-fill`, `--no-rendered-text`, `--no-skipped`, and `--no-meta`
  are optional boolean disables. Supplying the flag with no value means `true`;
  supplying `--no-coverage-fill=false` explicitly asks to keep coverage fill
  enabled when a profile default would otherwise omit it.
- `--explain-performance` replaces the normal context/search/pack payload with
  a redaction-safe performance report. It should not be combined with streaming
  context output.

## Graph Freshness And Filters

Graph read commands first apply the requested graph filters, then apply command
specific requirements such as freshness.

Worked examples:

```bash
ee graph centrality --workspace . --algorithm pagerank --limit 10 --json
ee graph centrality --workspace . --algorithm hits-hubs \
  --memory-id mem_release_policy --require-fresh --json
ee graph pagerank --workspace . --min-weight 0.2 --min-confidence 0.5 \
  --include-tombstoned --json
```

Precedence:

- `--min-weight`, `--min-confidence`, and `--link-limit` restrict the graph
  source rows before the algorithm result is emitted.
- `--include-tombstoned` changes graph/search visibility only; it is not an
  untombstone operation and does not modify lifecycle audit state.
- `--require-fresh` is stricter than ordinary stale-result rendering. When the
  latest persisted centrality snapshot is stale, the command must fail closed
  instead of silently returning stale scores.

## Curation And Maintenance

Curation and maintenance commands separate planning from mutation. The safe
default is to inspect or dry-run first, then apply only when the operator wants
the audited write.

Worked examples:

```bash
ee curate disposition --workspace . --no-structural-decay \
  --now 2026-05-19T00:00:00Z --json

ee maintenance run --workspace . --job decay_sweep \
  --no-structural-decay --dry-run --json

ee curate tombstone mem_old_rule --workspace . \
  --reason "superseded by validated release rule" --dry-run --json

ee curate tombstone mem_load_bearing_rule --workspace . \
  --allow-tombstone-load-bearing \
  --reason "operator reviewed load-bearing graph evidence" --dry-run --json

ee curate apply cand_retract_stale_rule --workspace . \
  --allow-tombstone-load-bearing --dry-run --json
```

Precedence:

- `--no-structural-decay` disables graph-structural decay adjustments for the
  specific disposition or maintenance run. It does not disable auditing or
  non-structural lifecycle rules.
- `--dry-run` keeps supported curation and maintenance commands in planning
  mode. Omit it only when the command is intentionally allowed to write.
- Load-bearing protection is the strongest read-side curation guard. If a
  candidate is also articulation-sensitive or onion-layer-protected, the
  operator should resolve the load-bearing review first.
- `--allow-tombstone-load-bearing` is the explicit override for applying a
  tombstone or retraction to load-bearing memories. It does not disable
  articulation or onion-layer reporting; it records that the operator reviewed
  the highest-precedence guard before continuing.
- `ee curate tombstone <id>` remains the explicit audited lifecycle mutation.
  Graph/search include flags such as `--include-tombstoned` only change read
  visibility and never untombstone or authorize a tombstone.

## Contract Hooks

Future contract coverage should pin these examples:

- help text mentions `--no-pack-dna` suppressing Pack DNA even with `--explain`;
- parser coverage accepts `--no-coverage-fill=false` for pack/context commands;
- renderer coverage shows `--json` overriding Mermaid-oriented output to JSON;
- graph centrality coverage treats `--require-fresh` as stricter than stale
  metadata rendering;
- docs coverage keeps load-bearing override examples limited to the two
  commands that currently expose `--allow-tombstone-load-bearing`.
