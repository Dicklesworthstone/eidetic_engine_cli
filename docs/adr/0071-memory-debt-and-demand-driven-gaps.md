# ADR 0071: Memory Debt and Demand-Driven Knowledge Gaps

Status: proposed
Date: 2026-06-10
Bead: bd-3ap2m.1 (epic bd-3ap2m, 2026-06 idea-wizard wave)

## Context

`ee doctor` diagnoses the ENVIRONMENT (DB, indexes, models); nothing
diagnoses the CONTENT. Every long-lived memory store rots, and the
difference between a tool trusted for a year and one abandoned in a month
is whether rot is visible and cheap to fix. This ADR fixes the contracts
for `ee curate doctor` (a deterministic memory-debt report with a
prioritized, actionable queue), `ee learn gaps` (knowledge-gap mining from
the query-miss ledger — what agents keep asking that memory cannot answer),
and the steward trend job that proves hygiene works. Fact-checked baseline
(2026-06-10): `QUERY_MISS_AUDIT_TTL_SECONDS` = 7 days, sample rate 1.0,
schema `ee.search.query_miss.v1`; `evaluate_memory_decay_with_settings` is
exported from `src/policy`. Cross-wiring: ADR 0067 makes ask abstentions
emit miss rows tagged `origin=ask`, so gaps cover ask demand as well as
search demand. Boundary table (the five surfaces that WILL be confused):
`ee doctor` = environment; `ee curate doctor` = content; insights
`knowledgeGaps` = graph-structural holes; `ee learn gaps` = demand-driven
holes; `ee learn agenda` = experiment targets.

## Decision

### 1. The six debt classes (deterministic formulas, persisted inputs only)

Inputs are strictly persisted rows — anchor freshness states, the conflict
surface, read audit rows (cursor-bounded scans), memory_links, feedback
stats, and `evaluate_memory_decay_with_settings` projections. No graph
recomputation, no LLM judgment.

| Class | Detector (deterministic) | Severity mapping | Suggested action (must classify Actionable) |
|---|---|---|---|
| `stale_anchor` | anchored memory with freshness `suspect`/`stale` | suspect→low, stale→warning | `ee recall --stale` review / `ee index rebuild` |
| `contradicted_unresolved` | pair on the conflict surface older than `[debt] conflict_age_days` (default 14) | warning; high if either side is procedural | `ee conflict resolve <a> <b> --verb … --apply` |
| `never_retrieved` | no `search.returned_mem`/`pack.included_mem` audit row in `[debt] retrieval_window_days` (default 60) AND older than the window | low | `ee curate disposition` review |
| `orphan` | no links AND no retrievals in window AND utility < 0.3 | low | tombstone candidate via `ee curate` |
| `low_trust_high_rank` | trust ∈ {cass_evidence, agent_assertion} AND pack-inclusion count ≥ 3 in window AND zero outcome events | warning (the misinformation-risk surface) | grade it: `ee outcome <id> --signal …` solicitation |
| `decay_imminent_high_utility` | decay projection says demote/tombstone within `[debt] horizon_days` (default 14) AND utility ≥ 0.6 with recent helpful outcomes | warning (half-life misconfiguration evidence) | `[learn.decay]` half-life review w/ pre-filled key |

- Composite debt score = severity-weighted count, normalized per 1k
  memories. Queue ordering: severity desc, then impact proxy
  (retrieval frequency × confidence) desc, then memory_id.
- Every suggested action must classify as **Actionable** under the
  degraded-honesty repair classifier (`classify_repair_command`) — a debt
  class without an executable repair is a report nobody acts on; this is a
  unit-tested invariant, not a guideline.
- `ee curate doctor [--class <name>] [--limit N] [--trend]` is read-only;
  mutations happen only through the suggested commands' own audited
  surfaces. Governor truncation point: the queue array.

### 2. Gap mining (`ee learn gaps`)

- **Retention**: `[search] query_miss_retention_days` default raised
  7 → 30 (weekly-cadence users lose the signal at 7); TTL enforcement
  moves to the explicit maintenance path with the new bound. Privacy
  posture unchanged and re-affirmed: miss rows keep hashed/redacted query
  text only, and the raise extends retention of THAT, not of raw queries.
- Pipeline: load miss rows + weak-recall degradation events in the window
  → normalize queries (lowercase, whitespace-collapse, stopword-light —
  the same deterministic normalization as ask's lexical overlap) → cluster
  with the HashEmbedder agglomerative machinery under the learn coherence
  threshold → rank clusters by demand (count × recency decay).
- Per-cluster output: demand stats **per origin** (`search | ask`; legacy
  rows default `search`), representative redacted queries,
  `nearestExistingEvidence` (top current hits below the relevance floor —
  WHY it missed), and a `rememberTemplate {suggestedLevel, suggestedKind,
  suggestedTags, contentSkeleton}` derived from documented query-shape
  rules (how-do-I → procedural/command; what-broke → episodic/failure;
  which/what-is → semantic/fact). A gap is one paste away from closed.
- Cross-link, don't duplicate: a cluster matching an open learn-agenda
  item annotates it instead of emitting a new gap row.
- Wire-through: `ee orient` carries a bounded gaps count + the suggested
  command (same pattern as decide-revisit and undistilled-journal
  surfacing).

### 3. Trend (`debt_snapshots` + steward job)

- `debt_snapshots` table: per-class counts + composite, db generation,
  created_at; bounded retention (`[debt] snapshot_retention_days`, default
  180). Written ONLY by the bounded steward job `memory-debt-snapshot`,
  chained AFTER the maintenance decay run so the trend measures
  post-maintenance reality. Idempotent per (generation, day).
- `ee curate doctor --trend` reads snapshots and reports per-class
  direction — two snapshots showing debt declining after hygiene actions
  is the evidence the whole memory system works.

### 4. Degradation vocabulary

| Code | Severity | Class | Trigger |
|---|---|---|---|
| `memory_debt_audit_window_partial` | info | response_time | audit rows pruned inside the window; scores computed on the partial window (stable, labeled) |
| `learn_gaps_no_miss_data` | info | response_time | empty miss ledger in scope (honest empty) |
| `learn_gaps_retention_short` | info | response_time | requested `--since` predates the retention bound |

Fixture/taxonomy files land with the emitting commits (bd-3ap2m.2/.3).
Schemas `ee.curate.doctor.v1`, `ee.learn.gaps.v1`, `ee.curate.debt_trend.v1`
ship standalone with those commits; shapes below are normative.

### 5. Performance bound

Full debt report on the 14k-memory perf-class fixture: p50 < 2 s
(`ee.perf.v1` advisory row, bd-3ap2m.2). Cursor-bounded audit scans are the
load-bearing mechanism — no unbounded table walks.

## Consequences

- **Easier**: vague distrust becomes a ranked queue with executable
  repairs; silent retrieval failures become capture prompts with
  pre-filled templates; the trend proves (or disproves) hygiene value.
- **Guarded**: read-only report; every mutation routes through existing
  audited surfaces; deterministic scoring with stable ordering; privacy
  posture of miss rows preserved through the retention raise.
- **Costs accepted**: one snapshot table + one steward job; the
  low_trust_high_rank detector will nag until outcomes are recorded —
  intentional, that nag is the misinformation-risk control.

## Rejected Alternatives

- **LLM judgment of memory quality**: non-deterministic, paid-API.
  Rejected for closed-form detectors.
- **Folding debt into `ee doctor`**: environment vs content separation
  keeps doctor's posture contract stable and its repair semantics
  (file/index fixes) distinct from content curation. Rejected.
- **Auto-executing suggested repairs**: violates no-silent-mutation;
  the queue suggests, agents apply. Rejected.
- **Raw query text in gap reports**: privacy regression; rejected —
  hashed/redacted representatives only.
- **Graph-structural gap detection here**: that is insights
  `knowledgeGaps` territory (bd-2pos6); this surface is demand-driven by
  design and the boundary table keeps them distinct.

## Verification

- Unit (bd-3ap2m.2): each detector on planted micro-fixtures
  (positive/negative/boundary); composite ordering determinism;
  partial-window honesty (prune rows → labeled degradation, stable
  scores); suggested-action Actionable-classification invariant; trend
  row shape + idempotent job re-run.
- Unit (bd-3ap2m.3): normalization/cluster determinism; demand ranking +
  tie-breaks; template inference per query-shape fixture; redaction of
  planted secret-bearing queries; empty-ledger honesty; agenda
  cross-link; origin split (search vs ask).
- Property (bd-3ap2m.4): insertion-order permutation never changes the
  report; resolving a planted debt item via its own suggested command
  strictly lowers that class count on re-run (monotonicity).
- E2E (bd-3ap2m.4): `scripts/e2e_memory_debt.sh` — aging-corpus generator
  plants all six classes + a healthy control + repeated missed queries;
  curate doctor finds every planted class with Actionable commands; one
  suggested action per class is EXECUTED through the real CLI and the
  queue shrinks correctly; steward snapshot twice around the fixes
  asserts --trend direction; learn gaps clusters the planted queries,
  a pasted template clears its cluster on the next run.
  `ee.test_event.v1` logging throughout.

## Appendix: report shapes (normative drafts)

```text
object ee.curate.doctor.v1 (under data.debt)
  schema       const "ee.curate.doctor.v1"
  generatedAt  rfc3339
  dbGeneration integer
  summary      {perClass: {<class>: integer}, composite: number,
                memoriesScanned: integer, windowDays: object}
  queue[]      (governor truncation point)
    memoryId     string (or pairIds for contradicted_unresolved)
    class        string
    severity     "low"|"warning"|"high"
    impactProxy  number
    evidence     object        (class-specific, persisted refs)
    suggested    {command: string, kind: "Actionable"}

object ee.learn.gaps.v1 (under data.gaps)
  clusters[]   (governor truncation point)
    demand          {total: integer, byOrigin: {search: integer, ask: integer},
                     recencyDecayed: number}
    representatives string[]   (hashed/redacted)
    nearestExistingEvidence [{memoryId, score}]
    rememberTemplate {suggestedLevel, suggestedKind, suggestedTags[],
                      contentSkeleton}
    agendaItemId    string | null   (cross-link instead of duplicate)

object ee.curate.debt_trend.v1 (under data.trend)
  points[]: {at: rfc3339, dbGeneration: integer,
             perClass: object, composite: number}
  direction: {<class>: "improving"|"flat"|"worsening"}
```
