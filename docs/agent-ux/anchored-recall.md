# Code-Anchored Recall

`ee recall` answers the narrow pre-edit question: "I am about to touch this
path, symbol, or diff; what stored memories are anchored to it?" It is not a
replacement for free-text retrieval or task packing.

| Need | Use |
|---|---|
| Known path, symbol, or changed-file set before editing | `ee recall` |
| Open-ended question with text terms | `ee search` |
| Task-wide context with provenance and explanations | `ee pack` |
| Full surface impact plus fallback search hits | `ee impact` |

Run recall before the first edit to a known surface:

```bash
ee recall --path src/db/mod.rs --workspace . --budget-tokens 400 --format markdown
```

For a diff-oriented change, use the read-only git selector:

```bash
ee recall --diff HEAD --workspace . --budget-tokens 400 --json
ee recall --diff-staged --workspace . --budget-tokens 400 --json
```

Selectors compose as OR within the surface set and deduplicate by memory id.
`--path <glob>` matches normalized workspace-relative paths with
case-sensitive fnmatch-style semantics. `--symbol <name>` is an exact symbol
match. `--kind`, `--level`, and `--stale` filter the anchored result set before
ranking. Use `--cursor` only with the continuation cursor returned by a
budget-truncated response.

`--budget-tokens` must be greater than zero. If a positive budget is still too
small to fit the next ranked recall item, the response returns an empty page with
`output_budget_unsatisfiable` and no continuation cursor; raise the budget or
omit the flag rather than retrying the same cursor sequence.

## Hook Install

Hook commands default to printing a managed plan. Use `--print` first, then
install only after reviewing the target path and generated snippets.

```bash
ee hook claude-code --print --workspace . --json
ee hook claude-code --install --workspace . --json
ee hook claude-code --undo --workspace . --json

ee hook codex --print --workspace . --json
ee hook codex --install --workspace . --json
ee hook codex --undo --workspace . --json
```

The install report uses `ee.hook.harness_install.v1`. It includes the harness,
mode, written paths, backup path, snippets, plan entries, and capability gaps.
The managed snippets inject a small recall block before edits and capture
failure context after commands when the harness supports those events. Unsupported
targets return capability gaps; keep recall as an explicit pre-edit command for
those harnesses.

## Latency

Recall sits on the pre-edit path, so it must stay cheap. ADR 0064 sets a warm
core target of less than 30 ms on the mac-m3-pro fixture class and a hook budget
around 400 output tokens. Hook integrations must fail open: if recall is slow,
returns an error envelope, or cannot read the requested diff, the edit proceeds
without injected recall text and the degraded code is left in the report.

## Stale Anchors

The reverse lookup table is `memory_anchor_index`. It is a derived,
rebuildable asset populated from the same anchor extraction used by search
documents. The durable source of truth remains the memory and anchor records.

Use stale-only recall to inspect anchors that need attention:

```bash
ee recall --stale --path src/db/mod.rs --workspace . --json
```

Repair stale or missing derived rows with the index rebuild path:

```bash
ee index rebuild --workspace . --json
ee recall --path src/db/mod.rs --workspace . --json
```

If a command exits 8 with `migration_required`, the database schema is older
than the binary. Apply migrations before relying on recall:

```bash
ee migrate status --workspace . --json
ee migrate run --workspace . --json
```

Dropping `memory_anchor_index` is not data loss, but do not repair it by
manual SQL. Use `ee migrate run` for schema drift and `ee index rebuild` for
derived-index drift so generation, audit, and repair posture stay coherent.

## Contracts

`ee recall` returns the standard `ee.response.v2` envelope. The recall payload
is under `data.recall` and declares `schema: "ee.recall.v1"`.

```bash
ee schema export ee.recall.v1 --json
ee schema export ee.hook.harness_install.v1 --json
```

Budget truncation uses the shared governor code `output_truncated_budget`, not
a recall-specific code. The continuation cursor is a recall cursor carried in
the degraded entry details and echoed by the recall payload.

## Degraded Codes

| Code | Meaning | Agent action |
|---|---|---|
| `anchor_index_empty` | No reverse-index rows exist for this workspace | Continue without recall context; rebuild only after anchored memories exist |
| `anchor_index_stale` | Reverse-index generation is behind DB generation | Run `ee index rebuild --workspace .` |
| `recall_filtered_empty` | Anchors matched, but filters removed all rows | Relax `--kind`, `--level`, or `--stale` |
| `recall_git_unavailable` | Read-only git diff failed | Retry with explicit `--path` selectors |
| `output_truncated_budget` | Whole recall items were dropped to honor the token budget | Re-run with `--cursor` or raise `--budget-tokens` |
