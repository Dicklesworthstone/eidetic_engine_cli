# ADR 0064: Code-Anchored Recall

Status: proposed
Date: 2026-06-10
Bead: bd-u875s.1 (epic bd-u875s, 2026-06 idea-wizard wave)

## Context

ee already extracts code anchors into search-document metadata
(`attach_memory_anchor_metadata` in `src/search/mod.rs` emits
`memory_anchor_kinds/hashes/freshness`) and ADR 0056 scores anchor freshness
drift — but no surface answers "I am about to touch `src/db/mod.rs`; what
does this workspace know about it?". Retrieval requires intent, so the
memories that would have prevented a mistake are never seen. `ee recall` is
the reverse lookup from code surface (path, symbol, git diff) to anchored
memories, designed to run inside a pre-edit harness hook (bd-u875s.4) at
<50 ms end-to-end and ~400 output tokens. This ADR fixes the reverse-index
storage, lookup semantics, ranking objective, response contract, and
degradation vocabulary before implementation (bd-u875s.2/.3).

## Decision

### 1. Reverse index: `memory_anchor_index` (derived, rebuildable)

A new SQLModel-migrated table — a **derived asset**, never a second source
of truth:

- Columns: `anchor_kind`, `anchor_hash`, `normalized_path` (workspace-
  relative, `/`-separated, no leading `./`), `symbol` (nullable),
  `memory_id`, `freshness_state` (`current | suspect | stale`, mirroring
  ADR 0056), `generation`.
- Populated from the **same anchor extraction** the search-document builder
  uses (single extractor, two consumers — drift between them is impossible
  by construction).
- Maintenance rides the existing `search_index_jobs` pathway: rows are
  rewritten on memory create/revise/tombstone, and the whole index advances
  with the search generation. `ee index rebuild` rebuilds it from scratch;
  losing it is an inconvenience, not data loss.
- Indexed for the two hot lookups: `(workspace_id, normalized_path)` and
  `(workspace_id, symbol)`.
- **Latency contract**: recall core p50 < 30 ms warm on the mac-m3-pro
  fixture class (it sits on the pre-edit hook path; a slow recall gets
  uninstalled). If the table path cannot meet this through the normal DB
  open path, the recorded fallback design is a small mmap'd sidecar
  materialized at index time — but the table is tried first; the sidecar is
  a follow-up bead, not v1 scope.

### 2. Lookup semantics

- `--path <glob>` (repeatable): exact path or glob; matching is performed
  against `normalized_path` with case-sensitive `fnmatch`-style semantics;
  multiple values compose as OR with result dedup by memory id.
- `--symbol <name>` (repeatable): exact symbol-name match (OR-composed).
- `--diff <ref>` / `--diff-staged`: shells out to git **read-only** to
  extract the changed path set (and hunk ranges, reserved for future
  span-level matching); git failure degrades (`git_unavailable`-family
  code), never blocks.
- `--kind <k>` / `--level <l>` (repeatable, pass-2 addition): filter the
  anchored result set BEFORE ranking. Narrow hooks need this — e.g. a
  pre-Bash hook wants only `failure | risk | anti-pattern` memories for the
  touched paths. Filters compose conjunctively with the surface lookup.

### 3. Ranking objective (deterministic)

```text
score(memory) = freshness_multiplier × confidence × level_tilt
  freshness_multiplier: current = 1.0, suspect = 0.7, stale = 0.4
                        (the ADR 0056 constants, reused not redefined)
  level_tilt:           procedural 1.0, semantic 0.8, episodic 0.6,
                        working 0.3; kind bonus ×1.15 for
                        failure | risk | anti-pattern (warnings first)
  tie-break:            memory_id ascending (stable, byte-deterministic)
```

Bounded candidate scan (the per-path/per-symbol index lookups are already
narrow); token budgeting via the ADR 0063 governor — recall declares its
truncation point as `data.recall.items[]`.

### 4. Response contract: `ee.recall.v1`

Standard `ee.response.v2` envelope; `data.recall` carries items ranked per
§3, each with: `memoryId`, `anchor {kind, path, symbol}`, `freshnessState`,
`scoreComponents {freshness, confidence, levelTilt, kindBonus}`, `level`,
`kind`, `contentPreview` (the existing 240-char single-line preview),
`provenance[]`, and `tags[]`. Markdown format renders a token-tight prepend
block with per-item provenance, mirroring pack markdown discipline.
`--stale` filters to `suspect | stale` items and appends repair hints — the
agent-facing view of what ADR 0056 today penalizes silently.

### 5. Degradation vocabulary

| Code | Severity | Class | Trigger |
|---|---|---|---|
| `anchor_index_empty` | info | response_time | the reverse index has no rows for this workspace (nothing anchored yet) — never a hard error |
| `anchor_index_stale` | low | response_time | reverse-index generation < DB generation; repair: `ee index rebuild --workspace .` |
| `recall_filtered_empty` | info | response_time | the index HAD anchored rows but `--kind/--level` filters removed them all (distinct from empty-index so hook authors can tell the difference) |

Budget truncation is **not** a recall-specific code: it is the governor's
`output_truncated_budget` (ADR 0063) with its `continuationCursor`. The
`recall_budget_truncated` code named in early planning (bd-u875s.1 body,
pass 1) is **superseded by this ADR** — one truncation vocabulary across
all surfaces. Fixture/taxonomy files for the three recall codes land with
the emitting commits (bd-u875s.2/.3) per the same-commit rule.

## Consequences

- **Easier**: memory becomes ambient — the bd-u875s.4 PreToolUse hook can
  inject anchored rules/failures before an edit with zero agent intent;
  `--stale` turns silent freshness decay into an actionable review queue.
- **Guarded**: read-only surface; derived index (rebuildable, honest
  staleness); deterministic ranking; failure modes degrade rather than
  block (a recall error must never stop an edit — the hook contract in
  bd-u875s.4 swallows non-zero exits).
- **Costs accepted**: one more derived table to keep in generation lockstep
  (mitigated: same job pathway as the search index); symbol matching is
  exact-name only in v1 (qualified-path symbol selectors deferred).

## Rejected Alternatives

- **Query-time scan of the search index** (no reverse index): O(corpus)
  per hook invocation; cannot meet the <30 ms contract. Rejected.
- **Reverse index outside the DB** (sidecar file as primary): splits the
  derived-asset story and evades the generation/rebuild machinery; kept
  only as a recorded latency fallback (§1), not the design.
- **Reusing `ee search` with a path-filter flag**: search ranks by query
  relevance, not anchor freshness×confidence; recall has no query text and
  must not pay query-parsing/scoring cost on the hook path. Rejected.
- **Recall-specific budget-truncation code**: superseded by the ADR 0063
  governor vocabulary (§5).

## Verification

- Unit (bd-u875s.2): ranking determinism incl. tie-breaks; glob matching
  edges (empty glob, absolute vs relative, case sensitivity); stale-
  generation detection; tombstone exclusion; filter×tilt interaction and
  the two distinct empty-result codes.
- Property (bd-u875s.5): same inputs ⇒ same order; budget monotonicity
  (smaller governor ceiling yields a strict prefix).
- Golden/contract (bd-u875s.5): `ee.recall.v1` snaps both formats;
  schema-drift validation; fixtures for the three codes.
- E2E (bd-u875s.5): `scripts/e2e_recall_hooks.sh` — seeded anchored
  memories, `--path`/`--diff` against a real git repo, hook-install
  round-trip, failure-capture path; `ee.test_event.v1` logging per step.
- Bench (bd-u875s.2): recall group under `ee.perf.v1` (advisory) pinning
  the <30 ms warm target.

## Appendix: `ee.recall.v1` (normative draft)

Standalone `docs/schemas/ee.recall.v1.json` ships with bd-u875s.3
(`x-ee-status` `shipped:false` until then); this draft is normative.

```text
object ee.recall.v1 (under ee.response.v2 data.recall)
  schema        const "ee.recall.v1"
  query         {paths: string[], symbols: string[], diffRef: string|null,
                 kinds: string[], levels: string[], staleOnly: boolean}
  items[]
    memoryId        string
    anchor          {kind: string, path: string|null, symbol: string|null}
    freshnessState  "current"|"suspect"|"stale"
    scoreComponents {freshness: number, confidence: number,
                     levelTilt: number, kindBonus: number}
    score           number
    level           string
    kind            string
    contentPreview  string (≤240 chars, single line)
    provenance      [{uri: string, sourceType: string}]
    tags            string[]
    repair          string|null   (stale items: suggested next command)
  indexGeneration integer
  dbGeneration    integer
```
