# ADR 0069: Global Knowledge Lane

Status: proposed
Date: 2026-06-10
Bead: bd-1bfwa.1 (epic bd-1bfwa, 2026-06 idea-wizard wave)

## Context

The ee database is already user-global with workspaces as first-class rows,
yet every memory is workspace-scoped: the "never `git reset HEAD` in a
shared tree" lesson learned in project A is invisible in project B, so
agents relearn it the expensive way. Repetition across projects is the
strongest trust signal there is, and today it is invisible. The global lane
gives validated procedural knowledge an audited road between workspaces —
and gives `ee` day-one value in a brand-new repo, where primer and recall
can serve the user's universal conventions before any local memory exists.
Fact-checked baseline (2026-06-10): `MemoryScope::Global` ALREADY parses
(`src/models/query.rs`: self | team | global | workspace | verified |
swarm) — so this ADR's first job is an audit of what the existing value
does, and the design implements lane SEMANTICS behind it rather than
extending an enum. This ADR fixes storage, promotion, precedence,
consumption, isolation, and the mesh boundary before implementation
(bd-1bfwa.2/.3).

## Decision

### 0. Audit obligation (first implementation step, bd-1bfwa.2)

Before any migration, bd-1bfwa.2 must trace every read of
`MemoryScope::Global` end-to-end and record findings in the bead: expected
result is parsed-but-thin (no promotion path, no lane storage, no
precedence). Any discovered live behavior keyed to Global is preserved or
explicitly migrated with a documented decision — no silent semantic change
to an existing scope value.

### 1. Storage: scope column, copy-with-link promotion

- Memories gain a `scope` column (`workspace` default | `global`) plus
  `origin_workspace_id` (nullable) and a `derived_from` link on promoted
  rows. **No separate store, no global pseudo-workspace**: one audit
  machinery, one feedback machinery, one lifecycle.
- Promotion is **copy-with-link**: the workspace original remains intact
  (local provenance preserved); the global row is a distinct memory with
  its own feedback life, carrying `derived_from` → origin and
  `origin_workspace_id` provenance forever.

### 2. Promotion contract (`ee memory promote-global <id>`)

- Eligible: procedural + semantic rules, anti-patterns, conventions.
  NEVER episodic/working (evidence-before-promotion applies doubly across
  workspace boundaries).
- Evidence gate: trust class ≥ `agent_validated` OR `human_explicit`;
  configurable minimum outcome support (`[global_lane]
  min_outcome_support`, default 1 helpful/confirmation event). Failing the
  gate is a policy denial: exit 7 with structured missing-evidence reasons.
- Redaction re-screen at promotion at `standard`+ (workspace content may
  cite local paths or secret-adjacent text that was acceptable locally);
  refusal emits `global_promotion_redaction_refused` (warning) with the
  class reasons.
- Cross-workspace dedup at promotion time: if the global lane (or another
  workspace) already holds a near-duplicate (`[curation]
  duplicate_similarity` threshold), promotion emits a MERGE curation
  candidate binding both origins instead of a second global row.
- `demote-global` reverses with audit: the global row is tombstoned via
  the existing lifecycle; the origin row is untouched.
- Both verbs are dry-run by default with full plan output; `--apply`
  mutates (wave-wide write-verb convention). Audit rows
  (`memory.promote_global` / `memory.demote_global`) land on both sides.

### 3. Feedback backflow

Outcome events against a global row work unchanged (it IS a memory row) and
update the global row's confidence wherever recorded from. A
`contradiction` signal from any consuming workspace creates a REVIEW
curation candidate referencing all consuming-workspace evidence — it does
NOT silently demote the origin row (the origin's local truth may still hold
locally; the conflict is what needs human/agent review).

### 4. Consumption and precedence

- Surfaces that join the lane by default when enabled: search, pack,
  recall (ADR 0064), primer (ADR 0065). Insights and graph projections do
  NOT join in v1 — cross-workspace links would entangle workspace-local
  snapshot lifecycles (a future ADR if wanted).
- Every emitted lane item is labeled: `lane: "global"`,
  `originWorkspace: <id>` — on pack item provenance, search metadata,
  recall items, and primer line provenance. An agent always knows a memory
  came from elsewhere.
- **Precedence on overlap** (same content hash or duplicate-similarity
  overlap): the workspace row WINS; the global row is annotated as
  corroboration. **Contradiction across lanes**: neither silently wins —
  the pair routes to the conflict surface labeled by lane, and assembly
  emits `global_lane_conflict_deferred` (info). Pack assembly must never
  resolve cross-lane contradictions by rank — hiding exactly the
  disagreement the user must see (e.g. global says rebase-never, this
  workspace says rebase-always).
- `ee search --all-workspaces`: a read-only diagnostic listing matches
  across every registered workspace + the global lane, each row labeled by
  workspace. Explicitly NOT a pack input; governor truncation point
  declared.
- Scope semantics: `--memory-scope global` (existing value) selects
  lane-only retrieval; default scope includes the lane when enabled +
  participating; `--strict-scope` behavior is preserved.

### 5. Isolation (hard privacy boundary)

`[global_lane] participate = false` blocks BOTH contribute and consume for
a workspace, enforced at the **repository layer** (bd-1bfwa.2) — not in CLI
argument handling — so no surface can leak around it. This is the
legal/medical isolation story (README "Beyond Coding"): privileged
workspaces opt out entirely. `[global_lane] enabled` is the store-wide kill
switch; `global_lane_disabled` (build_time/config class) reports posture.

### 6. Explicit non-goal: mesh

The lane lives WITHIN one user store across workspaces. Mesh
(machine-to-machine exchange) is governed separately by mesh peer policy;
nothing in this ADR changes mesh semantics, and lane rows cross machines
only by the existing mesh rules applied to them as ordinary memory rows.

### 7. Degradation vocabulary

| Code | Severity | Class | Trigger |
|---|---|---|---|
| `global_lane_disabled` | info | build_time/config | lane disabled store-wide or workspace not participating |
| `global_promotion_redaction_refused` | warning | response_time | promotion content failed standard+ re-screen |
| `global_lane_conflict_deferred` | info | response_time | cross-lane contradiction routed to conflict surface during assembly |

Fixture/taxonomy files land with the emitting commits (bd-1bfwa.2/.3).
Schema `ee.memory.global_promotion.v1` ships standalone with bd-1bfwa.2;
the shape below is normative.

## Consequences

- **Easier**: hard-won lessons travel; a rule observed in three workspaces
  carries that corroboration visibly; new repos start with the user's
  universal conventions via primer/recall.
- **Guarded**: evidence-gated, redaction-re-screened, audited promotion;
  copy-with-link keeps origins intact; precedence keeps local truth
  dominant; isolation is structural; backflow reviews instead of silently
  demoting.
- **Costs accepted**: one scope column + linkage; cross-lane conflicts add
  review work (intentional — that review IS the safety mechanism).

## Rejected Alternatives

- **Marking origin rows globally-visible in place**: feedback/decay/
  redaction would serve two masters with one row. Rejected for
  copy-with-link.
- **A separate global DB**: splits source of truth, duplicates audit and
  feedback machinery. Rejected.
- **Adding a new scope enum value**: superseded by the fact-check — the
  value exists; semantics are what is missing.
- **Rank-based cross-lane conflict resolution in pack assembly**: hides
  disagreement; rejected for conflict-surface routing.
- **Graph-lane joins in v1**: snapshot lifecycle entanglement; deferred to
  a future ADR with its own design.

## Verification

- Unit (bd-1bfwa.2): evidence-gate matrix (each trust class × kind);
  redaction refusal; duplicate-merge candidate path; audit completeness on
  both rows; repository-layer opt-out enforcement (participate=false
  cannot contribute or consume); backflow contradiction candidate;
  Global-scope audit findings recorded.
- Unit (bd-1bfwa.3): scope parsing matrix incl. strict mode; precedence
  determinism on planted overlaps (insertion-order independent); lane +
  origin labels present on all four consuming surfaces.
- Property (bd-1bfwa.4): precedence always picks the workspace row and
  annotates corroboration for generated overlapping pairs.
- E2E (bd-1bfwa.4): `scripts/e2e_global_lane.sh` — three temp workspaces,
  one shared user DB: validate→promote in A (dry-run plan then apply);
  surface labeled in B via pack/recall/primer; harmful outcome in B moves
  the global row visibly from A; participate=false workspace C sees
  nothing both directions; near-identical promotion from B yields a MERGE
  candidate; unvalidated-episodic promotion exits 7; planted-secret
  promotion refused. `ee.test_event.v1` logging throughout.

## Appendix: `ee.memory.global_promotion.v1` (normative draft)

```text
object ee.memory.global_promotion.v1 (under data.promotion)
  schema             const "ee.memory.global_promotion.v1"
  action             "promote"|"demote"
  dryRun             boolean
  sourceMemoryId     string
  globalMemoryId     string | null      (null in dry-run promote)
  originWorkspaceId  string
  evidenceGate       {trustClass: string, outcomeSupport: integer,
                      passed: boolean, missing: string[]}
  redactionScreen    {level: "standard"|"strict", passed: boolean,
                      refusedClasses: string[]}
  dedup              {nearDuplicateId: string|null, similarity: number|null,
                      mergeCandidateId: string|null}
  auditIds           string[]           (both sides)
```
