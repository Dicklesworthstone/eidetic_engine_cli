# External-Derivation Candidate Operator Contract

> **Audience:** agents and humans who *use* `ee curate` to land memories that
> a producer derived from existing sources (other memories, persisted
> evidence spans). For the underlying design, see
> [ADR 0043](adr/0043-external-derivation-candidates.md).

## 1. What `create_derived_memory` is for

`ee` is a memory substrate. Its curation pipeline already mutates *existing*
memories — promote, supersede, tombstone, etc. The
`create_derived_memory` candidate shape is the **one generic surface for
proposing a brand-new memory derived from sources**, instead of mutating an
existing target.

Use it when:

- A reflection or review pass distilled a new fact, rule, or insight from
  one or more existing memories or persisted evidence spans, and you want
  that distillation to land as a first-class memory with provenance.
- An external producer (`ee review session --propose`, a reflection-handshake
  ingest, a vendor pipeline that emits structured candidates) needs to push
  a derived memory into the curation queue for human or automated review.

Do **not** use it for:

- Re-stating an existing memory (use `promote`, `supersede`, or `tombstone`).
- Inserting a memory directly from raw user input (use `ee remember`).
- Storing private chain-of-thought, raw LLM transcripts, or unredacted
  session blobs — `ee` rejects those at validate time.

`create_derived_memory` is **propose-only**: the producer files a structured
candidate, and `ee curate` is the only path that mutates real storage. The
producer must not bypass the candidate workflow.

## 2. The safe command order

Always run the lifecycle in this order. Every step except `propose-derived`
is idempotent-safe to re-run; preview is read-only.

```text
┌─ producer ──────────────────────────────────────────┐
│ 1. ee curate propose-derived  …    (creates pending) │
└──────────────────────────────────────────────────────┘
                  │
                  ▼
┌─ inspect ───────────────────────────────────────────┐
│ 2. ee curate show <id> --json      (preview & drift) │
└──────────────────────────────────────────────────────┘
                  │
                  ▼
┌─ validate ──────────────────────────────────────────┐
│ 3. ee curate validate <id> --json  (structural OK?)  │
└──────────────────────────────────────────────────────┘
                  │
                  ▼ (optional re-show after validate)
                  │
┌─ apply ─────────────────────────────────────────────┐
│ 4. ee curate apply <id> --json     (mints memory)    │
└──────────────────────────────────────────────────────┘
                  │
                  ▼
┌─ verify ────────────────────────────────────────────┐
│ 5. ee why <createdMemoryId> --json (provenance)      │
└──────────────────────────────────────────────────────┘
```

Reject path: at steps 2–4 you may decide the candidate should not land.
Run `ee curate reject <id> --json` (or the per-project `disposition`
spelling surfaced in `nextCommands`) to dispose of it with an audited
reason. Reject is also read-only with respect to memory state; only the
candidate row and audit record change.

## 3. Step-by-step: each command

### 3.1 `ee curate propose-derived` — file the candidate

```bash
ee curate propose-derived \
  --workspace . \
  --source-memory mem_01H... \
  --source-memory mem_01J... \
  --source-evidence-span ev_01K... \
  --level semantic \
  --kind insight \
  --content "Search latency p99 spikes correlate with the daily index merge job." \
  --tag retrieval --tag perf \
  --confidence 0.7 \
  --producer-kind reflection \
  --producer-model claude-opus-4-7 \
  --json
```

What it does:

- Inserts a pending row into `curation_candidates` with
  `candidate_type = 'create_derived_memory'` and
  `target_memory_id = NULL`.
- Persists canonical **source refs** (memory IDs and evidence-span IDs)
  plus a `memorySpec` describing the derived memory it *would* mint.
- Records producer metadata (`producer_kind`, `producer_model`,
  `producer_note`) on the candidate so future audits can attribute the
  proposal.
- Returns `nextCommands` pointing at `ee curate show / validate / apply`.

Required: at least one `--source-memory` or `--source-evidence-span`.

`--dry-run` previews the candidate package and validation result
without inserting. Use it before you have producer state pinned.

`producer-kind`, `producer-model`, `producer-note` are persisted under
`metadata.producer` on the candidate and (after apply) referenced from
the audit row. Set them; they are how attribution survives later
review.

### 3.2 `ee curate show <id> --json` — preview before mutation

`ee curate show` is the **only** safe inspection surface for an
external-derivation candidate. It must not insert memories, attach
spans, enqueue search jobs, or write applied audit rows.

```bash
ee curate show cand_01H... --workspace . --json
```

The JSON output for a `create_derived_memory` candidate carries:

| Field | Meaning |
|---|---|
| `targetMemoryId` | Always `null` for create-derived (contrast: target-mutating candidates always have a string here). |
| `status` | `pending`, `validated`, `applied`, `rejected`, etc. |
| `candidateType` | `create_derived_memory`. |
| `memorySpec` | The level/kind/content/tags/confidence/utility/importance/validity-window the future memory would carry. |
| `proposedContent` *or* safe preview | The body text, possibly redacted per workspace policy. If hidden, the policy reason is explicit. |
| `sourceRefs` | Canonical memory/evidence-span references with **content hashes**. |
| `sourceDriftStatus` | Whether any source has drifted since proposal (when checked). |
| `producer` | Producer kind, model, note, and any other registered producer metadata. |
| `validationStatus` | Whether the candidate has been validated; what blocked it if not. |
| `linkPlan` | The `DerivedFrom` (and similar) links that apply will write. |
| `evidenceAttachmentPlan` | The evidence spans apply will attach to the new memory. |
| `searchIndexPlan` | The search-index job(s) apply will enqueue. |
| `auditPreview` | Schema + redacted detail preview of the audit row apply will write. |
| `nextCommands` | Copyable commands for validate / apply / why / reject. |

Read `sourceDriftStatus` and `auditPreview` carefully before applying.
A non-empty drift signal means the sources have changed since the
candidate was filed and the producer should regenerate.

### 3.3 `ee curate validate <id>` — structural OK?

```bash
ee curate validate cand_01H... --workspace . --json
ee curate validate cand_01H... --workspace . --dry-run --json   # no audit row
```

Validation runs structural and policy checks *without* loading a
target memory. For `create_derived_memory`, this includes:

- All source memories exist, are in this workspace, and are not
  tombstoned.
- All source evidence-span ids exist and are not already attached to a
  different memory.
- The proposed `memorySpec` is well-formed (level, kind, content
  non-empty, confidence ∈ [0, 1], optional scores in range).
- The proposed `trustClass` is acceptable. The default for an
  external derivation is `agent_assertion` — see §5.
- Redaction policy does not strip the proposed body.

Validation approval is necessary but **not** sufficient for apply.
Apply re-checks every invariant inside its write transaction because
sources may drift between validate and apply (see §6).

### 3.4 `ee curate apply <id>` — mint the memory

```bash
ee curate apply cand_01H... --workspace . --json
ee curate apply cand_01H... --workspace . --dry-run --json   # preview only
```

For a `create_derived_memory` candidate, apply runs in a single write
transaction:

1. Re-validates every source memory id, workspace, tombstone state, and
   `contentHash`.
2. Re-validates every evidence-span id, workspace, `contentHash`, and
   that `memory_id` is still `NULL`.
3. Mints a new memory using `memorySpec`. The new memory's
   `trust_class` defaults to `agent_assertion`.
4. Writes `DerivedFrom` (and related) links from the new memory to
   each `--source-memory`.
5. Attaches each `--source-evidence-span` to the new memory.
6. Enqueues a search-index job for the new memory body.
7. Writes an audit row with chain-hash continuity. The audit row's
   `details` carries producer identity, source refs, source content
   hashes, validation status, and the link/attachment plan that was
   actually executed.
8. Marks the candidate `applied` with `createdMemoryId` recorded.

If any re-check fails — drift, tombstone, span already attached,
concurrent apply, transient DB busy — apply fails closed with a
structured error and recovery action; **no partial state is left**
(no orphan links, no attached spans, no audit row labeled `applied`,
no search index job enqueued). See §6 for the failure-mode catalog.

### 3.5 `ee why <createdMemoryId>` — verify provenance

```bash
ee why mem_01M... --workspace . --json
```

Use `ee why` after apply to confirm:

- The new memory exists at the expected level/kind/content.
- `derivedFrom` links point at the cited source memories.
- Attached evidence spans match the producer's plan.
- The audit row chain includes the apply entry and producer identity.

The `ee why` envelope explains storage, retrieval, and pack selection
for the memory; it is the canonical surface for confirming a derived
memory's provenance from a user's perspective.

### 3.6 `ee curate reject <id>` — safe disposition

If preview reveals drift, policy issues, or the candidate is simply
wrong, dispose of it with:

```bash
ee curate reject cand_01H... --workspace . --reason "source memory tombstoned upstream" --json
```

Reject is non-destructive with respect to memory state. It records an
audited disposition reason, marks the candidate `rejected`, and frees
any duplicate-grouping key the producer used. It does **not** delete
the candidate row (the audit trail must remain).

The exact reject/disposition spelling for your workspace is surfaced
in the `nextCommands` array of `ee curate show`; prefer the
project-local spelling rather than guessing.

## 4. Source ref shapes

`create_derived_memory` accepts two source ref kinds. Both are
canonical and required to validate before apply:

| Ref | CLI flag | Notes |
|---|---|---|
| Memory source | `--source-memory <memory-id>` | Repeatable. The producer cites an existing memory in the same workspace. Memory sources **can** produce `memory_links` rows (DerivedFrom edges). |
| Evidence span | `--source-evidence-span <ev-id>` | Repeatable. The producer cites a persisted evidence span (typically from a CASS import). Evidence-span sources **cannot** produce memory_links; they appear in the attachment plan instead. |

A single candidate may cite both kinds. The producer must record at
least one source ref; a candidate with zero sources fails validation.

Source refs carry **content hashes** (BLAKE3 of the canonical body).
Validation and apply compare those hashes against the live store; a
mismatch means the source drifted and the producer must regenerate
the candidate. Stale source packages cannot be salvaged by retrying
apply — see §6.

## 5. Why `targetMemoryId: null`

Every other `candidate_type` in `ee` points at an existing memory
via `target_memory_id`. `create_derived_memory` is the lone
exception: its identity is **source-derived**, not target-mutating.
Persisting a non-null `target_memory_id` for a create-derived
candidate is a schema-level error guarded by the
`curation_candidates` CHECK constraint
(`(candidate_type = 'create_derived_memory' AND target_memory_id IS NULL)
OR (candidate_type != 'create_derived_memory' AND target_memory_id IS NOT NULL)`).

When apply succeeds, `createdMemoryId` is recorded on the audit row
and on the candidate row's applied-state metadata. That created id —
not `target_memory_id` — is the link from candidate to landed memory.

If your tooling assumes every candidate has a string `targetMemoryId`,
it is **wrong** and will silently miss create-derived candidates.
Update list/preview consumers to allow `null`.

## 6. Trust class

The default `trust_class` for a memory landed via
`create_derived_memory` is `agent_assertion` (the enum variant
`TrustClass::AgentAssertion`). This is intentional: a derived memory
has been proposed by a producer and validated for structure, but the
content has not yet been corroborated by independent outcome
evidence.

A producer **may** propose a higher trust class in `memorySpec`, but
validation enforces the project's accept-list and rejects values that
require outcome evidence that is not on file. Outcome-graded trust
classes (e.g. `OutcomeValidated`) typically require subsequent CASS
evidence or feedback before promotion.

The practical implication: agents reading freshly-applied derived
memories should treat their assertions as a hypothesis until later
evidence corroborates them. Downstream retrieval already weights trust
class into ranking, so leaving the default `agent_assertion` is the
honest choice.

## 7. Validation and apply differ from target-mutating candidates

Both for the operator and for the implementation:

| Phase | Target-mutating (promote, supersede, …) | `create_derived_memory` |
|---|---|---|
| Validate | Loads target memory; checks invariants against current target state. | Does **not** load a target. Checks `memorySpec` well-formedness, source refs liveness/hashes, trust-class policy, redaction policy. |
| Apply | Mutates the target memory row, writes audit linking new + old state. | Mints a new memory, writes `DerivedFrom` links to source memories, attaches source evidence spans, enqueues search job, writes audit with producer + source attribution. |
| Audit | References target memory ID directly. | References created memory ID *and* every source ref; producer identity lives in `details.producer`. |
| `targetMemoryId` | Always set. | Always `null`. |
| Retry | Apply retry must converge on the same mutated target. | Apply retry must converge on the **same** `createdMemoryId` when state is consistent; otherwise must fail closed (see §8). |

This table is the contract. If a consumer flow assumes one column on
both candidate shapes, it is incorrect.

## 8. Audit, search, and provenance — where to look

When apply succeeds, the following artifacts exist:

- A **memory row** at `createdMemoryId` with the proposed
  level/kind/content/trust class.
- One **`memory_links` row** per `--source-memory`, with link type
  `DerivedFrom` (and any kind-specific link types defined by the
  producer).
- One **evidence-span attachment** per `--source-evidence-span`,
  pointing the span's `memory_id` at the new memory.
- One **audit row** (chain-hash continuous) whose `details` payload
  contains: `producer`, `sourceRefs` with content hashes,
  `validationStatus`, `linkPlan` (executed), `evidenceAttachmentPlan`
  (executed), `createdMemoryId`, and `searchIndexJobId`.
- One **search-index job** in the queue (or already executed,
  depending on indexer cadence) for the new memory's body.

Use `ee why <createdMemoryId>` for a high-level provenance view, and
`ee curate show <candidate-id>` after apply for the original
candidate-side record.

## 9. Contract caveats

These are load-bearing details that operators get wrong if they're
not flagged:

1. **Producer identity lives in derivation metadata + audit details,
   not in a global audit column.** There is no `producer_kind` column
   on the audit table; look in `details.producer.kind` and
   `details.producer.payload`. If your reporting flow filters audits
   by a top-level producer column, it will return zero rows for
   create-derived applies.

2. **Source content hashes are drift guards.** They are not a
   cosmetic field. Validation and apply compare live BLAKE3 hashes
   against the values stored on the candidate; a drift forces
   regeneration. There is no flag to bypass the check, and
   `--allow-tombstone-load-bearing` does not apply to
   create-derived candidates.

3. **Stale source packages must be regenerated, not retried.** If a
   source memory was updated or an evidence span was attached
   elsewhere between propose and apply, the producer must re-issue
   the candidate with the live source state. Retrying apply with the
   stale candidate will keep failing closed.

4. **Raw chain-of-thought is not accepted as `content`.** Validation
   rejects bodies that look like LLM reasoning traces (private
   "thinking" output, unredacted tool transcripts, etc.). The
   producer must distill its reasoning into a memory-grade statement
   before proposing.

5. **Preview is read-only.** `ee curate show` and the `--dry-run`
   modes of `ee curate validate` / `ee curate apply` **must** not
   create memories, links, evidence attachments, search jobs, or
   applied audit rows. If a `show`/`--dry-run` invocation mutates
   any of those, file it as a bug.

6. **Apply is atomic, not idempotent-by-default for partial state.**
   A successful apply will return the same `createdMemoryId` on
   replay (idempotent retry path). An apply that **failed** mid-flight
   does not leave a partial memory: there is nothing to roll forward.
   If your code path needs apply to be idempotent across producer
   restarts, persist the candidate id and re-issue against it; do
   not re-propose with the same source set.

## 10. Common failure modes

The failure-mode catalog lives under
`tests/fixtures/failure_modes/`; the most common codes for
create-derived candidates are:

| Code | Trigger | Recovery |
|---|---|---|
| `derived_memory_source_memory_tombstoned` | A source memory was tombstoned after propose. | Regenerate the candidate against live sources, or reject the candidate with an audited reason. |
| `derived_memory_source_drift` | A source's `contentHash` no longer matches. | Same as above. |
| `derived_memory_evidence_span_already_attached` | The evidence span was attached to a different memory after propose. | Regenerate without that span, or supersede the conflicting memory if the spans were genuinely co-evidence. |
| `derived_memory_trust_class_invalid` | The proposed trust class requires outcome evidence not on file. | Lower the proposed trust class to `agent_assertion`, or attach the missing outcome evidence first. |
| `derived_memory_concurrent_apply_conflict` | Two candidates raced to attach the same evidence span. | The losing apply fails closed; re-run `ee curate show` on the losing candidate to see which span conflicted, then regenerate. |
| `derived_memory_redaction_rejects_content` | Workspace redaction policy strips the proposed body. | Adjust the proposal to remove the redacted content; redaction policy is authoritative. |

Each code is documented at
`tests/fixtures/failure_modes/<code>.json` with severity, recovery
command, and message substrings.

## 11. Producers should propose only

The boundary is one-way: external producers (reflection ingest, review
session propose, vendor pipelines) **propose** candidates. They do not
directly create derived memories, attach spans, write audit rows, or
enqueue search jobs. Every mutation flows through `ee curate validate`
and `ee curate apply`.

If a producer needs to short-circuit this boundary — to "just create
the memory" without going through curation — that is a design bug.
File it as a bead and discuss the boundary; do not work around it.

## 12. Related references

- [ADR 0043 — External-derivation candidates](adr/0043-external-derivation-candidates.md) — design context, schema changes, rejected alternatives.
- `src/cli/mod.rs` — `CurateCommand::ProposeDerived` (CLI surface).
- `src/core/curate.rs` — `propose_derived_candidate`, validation, apply.
- `src/curate/mod.rs` — `CandidateType::CreateDerivedMemory` enum variant.
- `tests/fixtures/failure_modes/` — per-code failure fixtures with recovery.
- `docs/degraded_codes.md` — agent-readable degraded-code summary.
