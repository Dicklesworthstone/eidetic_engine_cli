# ADR 0062: Agent Journal Capture and Distillation

Status: proposed
Date: 2026-06-10
Bead: bd-1pi9m.1 (epic bd-1pi9m, 2026-06 idea-wizard wave)

## Context

Agents consume context reliably but produce memory unreliably: `ee remember`
demands a composed sentence, a level/kind decision, and a mid-task tool call,
so the highest-value evidence (failures, surprises, dead ends) evaporates at
session end. The capture epic (bd-1pi9m) closes this with `ee journal` — an
append-only, redaction-screened observation log with near-zero ceremony — and
`ee journal distill`, a deterministic, extractive promotion path that turns
journal entries into **curation candidates** (never direct memories). The
ambient-hooks epic (bd-u875s.4) feeds the journal automatically from failed
commands, so the journal must absorb hook-frequency writes safely and cheaply.
This ADR records the storage, retention, boundary, distillation, and
degradation decisions before implementation (bd-1pi9m.2/.3).

## Decision

### 1. Storage: `journal_entries` table in the workspace DB

One new SQLModel-migrated table; FrankenSQLite remains the single source of
truth (no sidecar files). Columns and bounds:

- `entry_id` — UUIDv7 (time-ordered within a process).
- `workspace_id` — FK to workspaces.
- `agent_name` — nullable; from `EE_AGENT_NAME` at append time.
- `session_key` — nullable free-form, ≤ 128 bytes; harnesses may pass a
  session/run identifier for later scoped distillation.
- `kind` — enum `observation | command_failure | surprise | note`.
- `source` — enum `hook | manual | stdin`.
- `body` — UTF-8, hard cap 16 KiB; oversize input is truncated
  **deterministically at the last char boundary ≤ cap** and reported via
  `journal_entry_truncated` (info). Truncation never errors.
- `structured` — bounded JSON sidecar for machine fields: `cmd` (≤ 2 KiB),
  `exit_code` (int), `cwd` (≤ 1 KiB), `paths[]` (≤ 16 entries × 1 KiB),
  `stderr_tail` (≤ 2 KiB); total serialized sidecar ≤ 8 KiB, validated per
  field with per-field error details.
- `redaction_report` — JSON summary of classes applied at write time.
- `instruction_risk` — stored risk grade from the policy screen (see §3).
- `created_at` — RFC 3339 UTC.
- `distilled_at` — nullable RFC 3339; set by `distill --apply` (idempotency
  guard).
- `tombstoned_at` — nullable; set by retention phase 1 (§5).

Provenance URI scheme registered by this ADR: `journal://<entry-id>` —
carried by every distilled proposal so promoted memories cite their raw
evidence forever.

### 2. The journal is NOT in the search index

Journal entries are deliberately excluded from Frankensearch documents:

- The append path must stay hook-fast (p50 < 10 ms warm); per-entry index
  jobs would dominate that budget.
- Raw observations are noise relative to curated memories; indexing them
  would pollute retrieval and the curation queue — the exact failure the
  level system exists to prevent.
- Promotion INTO the indexed store happens only through distillation →
  curation candidates → existing audited `curate apply` machinery.

Inspection is served by read surfaces instead: `ee journal list`
(`--session | --agent | --since | --kind | --undistilled`; newest-first,
deterministic order, governor truncation point on the entries array) and
`ee journal show <entry-id>` (full record incl. `structured` and
`redaction_report`).

### 3. Write path: screen before storage

Every `body` and `structured` field passes the policy redaction screen
(`screen_external_text_for_ingestion` in `src/policy`) **before** any byte is
persisted — secrets never reach disk; applied classes are recorded in
`redaction_report` and surfaced as `journal_redaction_applied` (info).
Instruction-like content (prompt-injection cues) is **stored but graded**
with the existing `InstructionRisk` vocabulary; entries at or above the
configured exclusion grade are skipped by distillation with a stable
abstention reason (`instruction_risk_excluded`) rather than blocked at
capture (capture must not lose evidence; promotion is where trust gates).

### 4. Append surfaces and batch semantics

- `ee journal append "<text>" [--kind k] [--cmd c --exit-code N] [--session s]`
  — one entry.
- `ee journal append --stdin` — JSONL, one entry object per line,
  schema-validated per line, **per-line independent persistence**: each line
  lands or reports independently (its own transaction), so one poisoned line
  cannot roll back a session flush. Response carries `results[]` with
  per-line `{status, entryId | errorCode}`. Bounds: ≤ 512 lines per
  invocation (explicit error beyond), per-line caps as §1.
- Exit codes: `0` if at least one line landed; `5` if all lines failed;
  usage errors remain `1`.

### 5. Retention: working-tier, explicit, two-phase (RULE 1)

Journal entries are raw ore, not the vault. Config: `[journal]
enabled = true`, `retention_days = 14` (env mirrors registered with the
implementing bead). Enforcement is **only** via an explicit steward job
(`journal-retention`, runnable through `ee job run` / `ee maintenance run`,
optional under the daemon), in two audited phases:

1. **Tombstone**: entries older than retention get `tombstoned_at` plus a
   `journal.entry.tombstone` audit row (batch-summarized with counts and the
   id range).
2. **Prune**: a later explicit pass deletes rows that are both tombstoned and
   past a grace window (default 7 days), emitting a `journal.retention_prune`
   audit summary row. Nothing is deleted silently; nothing is deleted in
   phase 1.

Undistilled entries inside the retention window are never touched; distilled
entries keep their provenance value only until prune — promoted memories
retain the `journal://` URI as a historical pointer (documented as
potentially-dangling after prune, like any external provenance).

### 6. Distillation: deterministic, extractive, candidates-only

Contract `ee.journal.distill.v1` (Appendix B). Scope selectors: `--session`,
`--agent`, `--since`, default all-undistilled in the workspace. Pipeline
(no LLM, fully deterministic):

1. Select undistilled, non-tombstoned entries in scope; drop
   instruction-risk-excluded entries into `abstentions[]`.
2. Group `command_failure` entries by **normalized command root**
   (basename of argv[0] + first subcommand token; paths, hashes, and
   numbers stripped) and exit code; refine groups with the existing
   HashEmbedder agglomerative clustering under
   `[learn] cluster_coherence_threshold`.
3. Emit proposals: a cluster of ≥ 2 becomes one episodic `kind=failure`
   candidate with typed fields (`family` from the command root, `cause`
   guessed from dominant `stderr_tail` tokens) and one `journal://` evidence
   URI per member; lone `surprise` entries and first-seen failure shapes
   become single episodic proposals; `note`/`observation` entries below the
   signal threshold abstain (`below_signal_threshold`).
4. Dedup every proposal against existing memories via the remember-time
   neighbor machinery (`[curation] duplicate_similarity`); a near-duplicate
   yields a **REINFORCE** proposal targeting the existing memory instead of
   a create proposal.
5. `--dry-run` (default) prints the full proposal set with evidence and
   abstentions, writing nothing. `--apply` writes curation candidates
   (status pending — review flows through the existing
   `ee curate candidates/validate/apply` surfaces), sets `distilled_at`, and
   writes one audit row per proposal. Re-running distill over distilled
   entries proposes nothing (idempotent by `distilled_at`).

A bounded steward job `journal-distill` runs the same pipeline under the job
ledger, lock discipline, `job_budget_ms`, and `&Cx` cancellation.

### 7. Degraded codes (pre-classified here; files land with emission)

| Code | Severity | Class | Trigger |
|---|---|---|---|
| `journal_disabled` | info | build_time/config | `[journal] enabled = false` or feature-gated build |
| `journal_entry_truncated` | info | response_time | body/sidecar exceeded caps; deterministic truncation applied |
| `journal_redaction_applied` | info | response_time | secret classes redacted before storage |
| `distill_no_candidates` | info | response_time | scope had entries but nothing met proposal thresholds (honest empty) |

Per the same-commit fixture rule, the
`tests/fixtures/failure_modes/<code>.json` files and the
`docs/degraded_code_taxonomy.md` rows land in the bd-1pi9m.2/.3 commits that
first emit each code — not with this ADR — so the J6 catalog validator and
the contract-drift radar stay atomic with emission.

### 8. Boundaries with neighboring subsystems

- **Flight recorder (`src/obs`)**: an observability event log for
  diagnostics/support bundles; hash-chained, never distilled, its retention
  serves debugging. The journal is *memory-bound evidence* destined for
  curation. A failed command may legitimately land in both, for different
  consumers.
- **`ee learn observe`**: records observations against an explicit learn
  item (experiment-scoped, hypothesis-driven). The journal is task-agnostic
  ambient capture; distillation may *cite* journal entries when proposing
  learn-adjacent memories, but the two stores do not merge.
- **`ee remember`**: the deliberate write path for already-classified,
  durable memory. The journal is pre-memory: unclassified, working-tier,
  promotable only through review. When an agent already knows the
  level/kind, `remember` remains the right call.

## Consequences

- **Easier**: hook-driven capture is free; a session that never calls
  `remember` still yields reviewable candidates; retention keeps the table
  small; every promoted memory carries raw-evidence provenance.
- **Guarded**: redaction before persistence; injection-graded entries cannot
  reach the procedural layer; no silent memory mutation anywhere (candidates
  only, two-phase audited retention per RULE 1).
- **Costs accepted**: one new table + two steward jobs; journal content is
  not directly searchable (intentional — see §2); `journal://` URIs may
  dangle after prune (documented).

## Rejected Alternatives

- **Write a working-level memory per observation**: floods retrieval and the
  curation queue, violates evidence-before-promotion, and pays index-job
  latency on the hook path. Rejected for distill-then-curate.
- **JSONL sidecar file outside the DB**: splits the source of truth, evades
  audit and redaction guarantees, and breaks under concurrent writers.
  Rejected for a SQLModel table.
- **Index journal entries in Frankensearch**: latency + noise (§2). Rejected.
- **LLM summarization in distill**: violates determinism and the
  no-paid-API principle. Rejected for extractive clustering + heuristics.
- **Auto-apply distilled memories**: violates no-silent-mutation. Rejected
  for pending curation candidates.
- **Silent TTL deletion**: violates RULE 1. Rejected for two-phase
  tombstone-then-prune with audit rows.

## Verification

- Unit (bd-1pi9m.2/.3): redaction-before-persistence (raw secret absent from
  DB file bytes), JSONL line-isolation, bounds/truncation edges,
  `EE_AGENT_NAME` attribution, clustering determinism, dedup-to-reinforce,
  instruction-risk exclusion, distill idempotency, dry-run zero-row assert.
- Property: arbitrary valid/invalid line interleavings preserve per-line
  status isolation.
- E2E (bd-1pi9m.6): `scripts/e2e_journal_capture.sh` — capture → list/show →
  distill dry-run → apply → curate validate/apply → search finds the
  distilled memory with `journal://` provenance; every step logs
  `ee.test_event.v1` lines; includes an exit-8 migration assert against a
  pre-migration DB.
- Bench (bd-1pi9m.6): journal group in `scripts/bench.sh` (append single,
  50-line batch, distill over 200-entry fixture) emitting `ee.perf.v1`.

## Appendix A: `ee.journal.entry.v1` (normative draft)

Standalone `docs/schemas/ee.journal.entry.v1.json` (with `x-ee-status`
`shipped:false` until bd-1pi9m.2 lands) and inventory registration ship with
the implementing bead; this draft is normative for field shape.

```text
object ee.journal.entry.v1
  schema        const "ee.journal.entry.v1"
  entryId       string (uuid)
  workspaceId   string
  agentName     string | null
  sessionKey    string | null            (≤128 bytes)
  kind          "observation"|"command_failure"|"surprise"|"note"
  source        "hook"|"manual"|"stdin"
  body          string                   (≤16 KiB post-truncation)
  structured    object | null
    cmd         string | null            (≤2 KiB)
    exitCode    integer | null
    cwd         string | null            (≤1 KiB)
    paths       string[] | null          (≤16 × ≤1 KiB)
    stderrTail  string | null            (≤2 KiB)
  redactionReport object                 {classesApplied: string[], spanCount: int}
  instructionRisk "none"|"low"|"medium"|"high"
  createdAt     string (rfc3339)
  distilledAt   string (rfc3339) | null
  tombstonedAt  string (rfc3339) | null
```

## Appendix B: `ee.journal.distill.v1` (normative draft)

```text
object ee.journal.distill.v1
  schema        const "ee.journal.distill.v1"
  scope         {session?: string, agent?: string, since?: rfc3339}
  dryRun        boolean
  proposals[]
    proposalId  string
    action      "create_candidate"|"reinforce_existing"
    targetMemoryId string | null         (reinforce only)
    level       "episodic"
    kind        "failure"|"fact"|"decision"|...
    contentDraft string
    typedFields object | null            (family, cause, ... per registry)
    evidence    string[]                 (journal://<entry-id>, ≥1)
    clusterSize integer
    dedup       {nearestMemoryId: string|null, similarity: number|null}
  abstentions[]
    entryId     string
    reason      "instruction_risk_excluded"|"below_signal_threshold"|"already_distilled"
  applied       {candidateIds: string[], auditIds: string[]} | null
  degraded[]    standard ee.response.v2 degraded entries
```
