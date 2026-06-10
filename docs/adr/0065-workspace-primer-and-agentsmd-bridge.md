# ADR 0065: Workspace Primer and AGENTS.md Bridge

Status: proposed
Date: 2026-06-10
Bead: bd-39tzu.1 (epic bd-39tzu, 2026-06 idea-wizard wave)

## Context

The cheapest session-start knowledge injection today is a static
CLAUDE.md/AGENTS.md that silently rots. `ee primer` is the memory analogue:
a deterministic, cached, budgeted (~600-token default) workspace charter
assembled from the highest-value durable memory — top procedural rules,
unresolved high-severity failures/anti-patterns, key decisions, and
load-bearing memories — every line provenance-backed, regenerated from
decayed/curated/outcome-weighted evidence instead of hand maintenance. The
companion bridge closes the loop with harness-native files: `ee export
agentsmd` maintains a marker-delimited managed section, `ee import agentsmd`
parses rule-like statements into curation candidates, and `ee diag
agentsmd-drift` reports contradictions between what the files claim and what
memory has learned. Primer is KNOWLEDGE; `ee swarm brief` is coordination;
`ee orient` is posture — the three must stay distinct (docs carry the
decision table). This ADR fixes selection, budgeting, caching, provenance,
bridge contracts, and degradation before implementation (bd-39tzu.2/.3/.4).

## Decision

### 1. Selection objective (task-independent, deterministic)

Primer assembly is a fixed quota mix over four sections, computed from
persisted state only (no graph algorithms inline):

| Section | Source + ordering | Default quota share |
|---|---|---|
| `rules` | procedural rules by confidence × utility × persisted graph authority (when centrality rows exist) | 40% |
| `warnings` | unresolved failure/anti-pattern/risk memories by severity × recency | 25% |
| `decisions` | decision-kind chain heads (not superseded) by recency | 20% |
| `loadBearing` | persisted articulation/authority rows joined to memories | 15% |

Deterministic tie-breaks by `memory_id`. Graph-derived inputs read
**persisted centrality rows only**; when rows are missing or stale the
section is honestly omitted and `primer_graph_unavailable` (info) is
emitted — never an inline recomputation (latency contract).

### 2. Budget mechanics

Token budgeting reuses the pack budgeting utilities (same tiktoken-rs
encoder as ADR 0063). Under tight budgets sections shrink proportionally
with a documented floor: **rules never drop to zero while any exist**; the
floor order is rules > warnings > decisions > loadBearing. Sweeping the
budget downward yields a selection subset, not a rewrite (golden-tested at
200/600/4000 tokens). `primer_budget_floor` (info) reports when floors
engaged. Default budget: `[primer] default_tokens = 600`.

### 3. Cache (persisted, byte-identical)

A `primer_cache` table keyed by `(workspace_id, db_generation, config_hash,
budget, format)` stores the rendered output. Cache hits are byte-identical
and report `meta.cacheHit = true`; any DB generation advance invalidates.
`--refresh` forces re-assembly (still deterministic ⇒ still byte-identical
to what a cold assembly produces). A bounded steward job `primer-refresh`
re-warms the cache after curation apply / decay maintenance so the next
session start is a cache hit. Cold path emits `primer_cache_cold` (info).
The cache is a derived asset: dropping the table is safe (cold cache only).

### 4. Provenance rendering

Markdown lines end with compact provenance refs (short memory-id form,
e.g. `[mem_01HQ3K5Z]`); JSON carries full provenance objects per item.
Redaction: memories whose bodies would require redaction above the
workspace `[privacy]` defaults are skipped with a counted skip reason in
`meta.skipped` — never leaked, never mangled.

### 5. AGENTS.md bridge contracts (consumed by bd-39tzu.4)

- **Export**: `ee export agentsmd` renders the primer `rules` + `warnings`
  sections into a managed block delimited by
  `<!-- ee:agentsmd:begin generation=<dbGeneration> -->` /
  `<!-- ee:agentsmd:end -->`, with per-line provenance comments. It NEVER
  edits outside its markers; before the first mutation of a file it writes
  `<file>.ee-backup` (RULE 1); it creates files only with explicit
  `--create`; `--dry-run` prints the diff. Idempotent: unchanged memory ⇒
  byte-identical block. A hand-edited managed block is detected by content
  hash and refused with `agentsmd_unmanaged_edit_detected` (warning) unless
  `--force-managed-block` is passed (the hand edit is preserved in the
  backup).
- **Import**: `ee import agentsmd` parses rule-like statements (imperative
  bullets, MUST/NEVER/ALWAYS sentences) OUTSIDE ee markers into curation
  candidates — kind `rule`/`convention` proposals, trust class capped at
  `agent_assertion`, provenance `file://<path>#L<n>` — candidates only,
  never direct memories; near-duplicates of existing rules become REINFORCE
  proposals (same dedup semantics as ADR 0062 distillation). Parser bias is
  precision over recall: a missed rule costs little; a false extraction
  pollutes the curation queue.
- **Drift diagnostic**: `ee diag agentsmd-drift` (read-only) reports three
  finding classes: stale export (managed-block generation < DB generation),
  file-vs-memory contradictions (file mandates X, high-confidence memory
  says not-X — routed through the conflict surface vocabulary), and
  memory rules absent from the file. Advisory output with suggested
  commands; never mutates.
- Boundary with `ee init` boilerplate: `init` generates static scaffold
  files; the bridge manages only its marker-delimited section inside
  whatever file exists. The two never fight because the bridge refuses
  unmarked territory.

### 6. Degradation vocabulary

| Code | Severity | Class | Trigger |
|---|---|---|---|
| `primer_cache_cold` | info | response_time | no cache row for the key; assembled fresh |
| `primer_graph_unavailable` | info | response_time | persisted centrality rows missing/stale; loadBearing omitted |
| `primer_budget_floor` | info | response_time | proportional shrink hit a section floor |
| `agentsmd_file_missing` | info | response_time | target file absent and `--create` not passed |
| `agentsmd_markers_missing` | info | response_time | file exists but has no managed block (import-only or first export) |
| `agentsmd_unmanaged_edit_detected` | warning | response_time | managed block hash mismatch; export refused without `--force-managed-block` |

Fixture/taxonomy files land with the emitting commits (bd-39tzu.2/.3/.4)
per the same-commit rule. Envelope budget truncation, where applicable, is
the ADR 0063 governor vocabulary.

## Consequences

- **Easier**: every session gets a current, provenance-backed charter for
  ~600 tokens; `ee orient --include-primer` makes cold-start one call;
  CLAUDE.md stops rotting because ee generates and audits it.
- **Guarded**: deterministic byte-identical output (cache or not); RULE-1
  backups before any file mutation; import is candidates-only with a trust
  cap; redaction skips are counted, never silent.
- **Costs accepted**: one derived cache table + a steward job; primer
  quality is bounded by curation quality (by design — the fix for a bad
  primer is curation, not primer-side heuristics).

## Rejected Alternatives

- **LLM summarization**: violates determinism + no-paid-API. Rejected for
  quota-based extractive selection.
- **Reusing `ee pack` with a fixed task string**: pack is task-conditioned
  and uncacheable across arbitrary phrasings; primer must be
  task-independent, stable, and cacheable. Rejected (pack budgeting
  utilities are reused; pack selection is not).
- **Editing AGENTS.md outside markers / no backup**: violates RULE 1 and
  invites clobbering hand-written content. Rejected for marker + backup +
  refusal-on-hand-edit.
- **Direct memory writes on import**: violates evidence-before-promotion.
  Rejected for capped curation candidates.
- **Inline graph computation for loadBearing**: breaks the latency contract
  and duplicates snapshot machinery. Rejected for persisted-rows-or-omit.

## Verification

- Unit (bd-39tzu.2): quota math under tight/typical/huge budgets;
  deterministic tie-breaks; cache-hit byte-identity; generation
  invalidation; graph-absent path; redaction skip accounting.
- Unit (bd-39tzu.4): marker idempotency (export twice ⇒ byte-identical),
  no-clobber outside markers, hand-edit refusal, parser precision/recall on
  a fixture AGENTS.md (this repo's own as a snapshot), import
  dedup-to-reinforce, drift contradiction detection.
- Golden (bd-39tzu.5): primer markdown+JSON snaps; byte-identity double-run
  golden; 200/600/4000 budget-sweep monotonicity goldens.
- E2E (bd-39tzu.5): `scripts/e2e_primer_agentsmd.sh` — cold/warm primer,
  generation invalidation, export markers + backup, hand-edit refusal,
  import to candidates with file provenance, drift diag findings; every
  step logs `ee.test_event.v1`.
- Bench (bd-39tzu.5): cold-assemble + warm-hit groups in `scripts/bench.sh`
  emitting `ee.perf.v1` (warm hit < 100 ms is the load-bearing number for
  the SessionStart hook recipe).

## Appendix: `ee.primer.v1` (normative draft)

Standalone `docs/schemas/ee.primer.v1.json` ships with bd-39tzu.2
(`x-ee-status` `shipped:false` until then); this draft is normative.

```text
object ee.primer.v1 (under ee.response.v2 data.primer)
  schema        const "ee.primer.v1"
  budgetTokens  integer
  format        "markdown"|"json"
  cacheHit      boolean
  dbGeneration  integer
  configHash    string
  sections[]
    name        "rules"|"warnings"|"decisions"|"loadBearing"
    items[]
      memoryId      string
      line          string        (rendered, provenance-suffixed in markdown)
      level         string
      kind          string
      confidence    number
      provenance    [{uri: string, sourceType: string}]
  meta
    tokensUsed    integer
    skipped       {redaction: integer, budgetFloor: integer}
    floorsEngaged string[]       (section names)
```
