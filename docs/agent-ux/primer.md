# Workspace Primer and the AGENTS.md Bridge

This guide is for coding agents (and the humans wiring them) using `ee primer`
and the AGENTS.md bridge surfaces. Treat `ee.primer.v1` and the
`ee.agentsmd.*.v1` schemas as the contract; ADR 0065 is the design record.

## What the primer is — and is not

`ee primer` emits a deterministic, cached, budgeted (~600-token default)
workspace charter assembled from the highest-value durable memory: top
procedural rules, unresolved high-severity warnings, key decisions, and
load-bearing memories. Every line is provenance-backed (`[mem_xxxxxxxx]`
short refs in markdown; full provenance objects in JSON). It is regenerated
from decayed/curated/outcome-weighted evidence, so it cannot silently rot the
way a hand-maintained CLAUDE.md does.

The primer is KNOWLEDGE. It deliberately does not overlap with:

| Surface | Question it answers | Conditioning | Cached |
|---|---|---|---|
| `ee primer` | "What does this workspace know that I must not violate?" | task-independent | yes (`primer_cache`, byte-identical hits) |
| `ee orient` | "What posture should I take for THIS task right now?" | task + flags | partially |
| `ee swarm brief` | "Who else is working here and what is in flight?" | live coordination state | no |
| `ee pack` | "What context should I read for THIS task?" | task-conditioned retrieval | replay ledger, not cache |

Pick the primer when you want stability and cacheability; pick `pack` when
you want task relevance. `orient --include-primer` gives you both posture and
charter in one call.

## Sections and budget mechanics

Fixed quota mix (ADR 0065 §1): `rules` 40%, `warnings` 25%, `decisions` 20%,
`loadBearing` 15%. Deterministic tie-breaks by memory id. Under tight
budgets, sections shrink proportionally with a floor order
`rules > warnings > decisions > loadBearing`; rules never drop to zero while
any exist (`primer_budget_floor`, info). A smaller budget yields a selection
subset of the larger budget, never a rewrite — golden-tested at 200/600/4000
tokens in `tests/primer_cli_golden.rs`.

`loadBearing` reads persisted centrality rows from the latest valid graph
snapshot only. When rows are missing the section is honestly omitted and
`primer_graph_unavailable` (info) is emitted with the repair
`ee graph centrality-refresh --workspace .` — never an inline recomputation.

## Cache and refresh semantics

The `primer_cache` table is keyed by
`(workspace_id, db_generation, config_hash, budget, format)`. Hits are
byte-identical to the cold assembly with only `cache_hit` flipped; any DB
generation advance (every memory write) invalidates. `--refresh` forces
re-assembly; `--no-persist` reads without warming. Cold assembly emits
`primer_cache_cold` (info) — informational, never retry on it: the call that
reported it just warmed the cache.

Measured on the `ee_primer` bench (mac-m3-pro class): cold assemble ~4.3 ms,
warm cache hit ~31 µs against the <100 ms SessionStart budget. The
`primer-refresh` steward job re-warms the cache after decay/curation so the
next session start is a hit.

## SessionStart hook recipe

One call, fast, fails open:

```bash
ee orient "<task>" --include-primer --fast --json
```

Read `data.primer.sections[]` for the charter and `data.primer.degraded[]`
for honesty signals. Standalone form for prompt prepends:

```bash
ee primer --workspace . --format markdown
```

## AGENTS.md bridge walkthrough

Three surfaces close the loop with harness-native files (`AGENTS.md` /
`CLAUDE.md`), all defaulting to `AGENTS.md` and accepting `--file <path>`.

### Export: own one block, never the file

```bash
ee export agentsmd --workspace . --create --json   # first run
ee export agentsmd --workspace . --json            # refresh
```

Export renders the primer `rules` + `warnings` sections into a managed block:

```text
<!-- ee:agentsmd:begin generation=<dbGeneration> hash=blake3:<16hex> -->
...rendered rules and warnings, one provenance ref per line...
<!-- ee:agentsmd:end -->
```

The managed-marker contract: export NEVER edits outside its markers; files
are created only under explicit `--create`; `--dry-run` prints the would-be
block diff and writes nothing. RULE-1 backup behavior: before any mutation of
an existing file, the full pre-mutation content is written to
`<file>.ee-backup`. Re-export with unchanged memory is byte-identical
(`changed: false`, no write, no backup).

The `hash=` attribute is a content hash of the block body. If someone edits
inside the markers, the next export refuses with
`agentsmd_unmanaged_edit_detected` (warning) and touches nothing; pass
`--force-managed-block` after reviewing with `--dry-run`, and the hand edit
is preserved in the backup. Content outside the markers is never at risk.

### Import: hand-written rules become candidates, never memories

```bash
ee import agentsmd --workspace . --json          # dry run (default)
ee import agentsmd --workspace . --apply --json
```

The parser is precision-first: uppercase hard modality (MUST / MUST NOT /
NEVER / ALWAYS / DO NOT) anywhere, leading cues (Always/Never/Don't/Avoid/
Prefer) on bullets only; it skips code fences, headings, tables, comments,
blockquotes, and the managed block (the bridge never re-imports its own
export). Extracted statements become pending curation candidates — kind
`rule`/`convention`, trust class capped at `agent_assertion`, provenance
`file://<path>#L<n>` with real evidence spans. Near-duplicates of existing
memories become reinforce proposals (same dedup semantics as journal
distillation). Candidate ids are text-keyed and deterministic, so re-apply
abstains with `already_imported` instead of double-inserting.

### Drift: audit the file against memory

```bash
ee diag agentsmd-drift --workspace . --json
```

Read-only and advisory. Three finding classes: stale export
(`managedBlock.stale` — block generation behind the DB generation),
file-vs-memory contradictions (`contradiction_link` signal: a hand-written
statement pairing with a high-confidence procedural rule at opposite
polarity), and memory rules absent from the file (`missingRules`).
`suggestedCommands` carries the follow-ups; the diagnostic never mutates.

### Keep-CLAUDE.md-honest in CI

```bash
ee export agentsmd --workspace . --dry-run --json \
  | jq -e '.data.changed == false'                  # fail CI when the block is stale
ee diag agentsmd-drift --workspace . --json \
  | jq -e '(.data.contradictions | length) == 0'    # fail CI on contradictions
```

## Degradation vocabulary

| Code | Severity | Meaning |
|---|---|---|
| `primer_cache_cold` | info | no cache row for this key; assembled fresh (and warmed) |
| `primer_graph_unavailable` | info | centrality rows missing; loadBearing honestly omitted |
| `primer_budget_floor` | info | tight budget hit the rules floor; lower-priority items evicted |
| `agentsmd_file_missing` | info | bridge target absent and `--create` not passed |
| `agentsmd_markers_missing` | info | file has no managed block yet (import-only or first export) |
| `agentsmd_unmanaged_edit_detected` | warning | block hash mismatch; export refuses without `--force-managed-block` |

Full trigger/repair details live in `docs/degraded_codes.md`; schemas in
`docs/schemas/ee.primer.v1.json` and `docs/schemas/ee.agentsmd.{export,import,drift}.v1.json`.
