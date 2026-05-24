# ADR 0043: External-derivation candidates

## Status

Proposed — 2026-05-22

## Context

ee's curation pipeline only mutates *existing* memories. The closed
`CandidateType` enum at `src/curate/mod.rs:363` names twelve mutation shapes —
`Consolidate`, `Promote`, `Deprecate`, `Supersede`, `Tombstone`, `Merge`,
`ParaphraseDedupProposal`, `Split`, `Retract`, `Rule`, `AntiPatternProposal`,
`Procedure` — and every one operates on a candidate's `target_memory_id`. The
DB schema enforces this at `src/db/mod.rs:5133`:

```sql
target_memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE
```

This blocks a real workflow that is already in flight. `ee review session
--propose` distills CASS evidence into proposed memories, but bootstrap
candidates that propose *new* memories — sourced from session spans, not from
an existing memory — cannot be persisted. The blocker is documented inline at
`src/core/curate.rs:1653` and tracked by bd-2d32o:

> bootstrap (propose_new_memory) candidates carry an empty target_memory_id
> sentinel because no existing memory has been linked yet. The
> curation_candidates table currently enforces a FK to memories.id, so
> persisting an empty string would fail. Skip persistence for bootstrap
> candidates and let the dry-run surface still return them […]. Persisting
> bootstrap candidates needs a schema follow-up to relax the FK or to
> materialize a placeholder memory at accept time; tracked for a downstream
> slice.

bd-2d32o closed with a dry-run workaround and deferred the schema fix.

The same gap will block any future "ingest a reflection result as a curation
candidate" surface (the reflection-handshake protocol explored separately) and
any other path where an external derivation produces a new memory from
sources.

## Decision

Introduce one generic candidate shape — `create_derived_memory` — that derives
a new memory from one or more typed source references. Sources may be existing
memories (`mem_...`) or persisted evidence spans (`ev_...`); this distinction
matters because only memory sources can produce `memory_links` rows. The source
list, derived-memory spec, producer identity, and any kind-specific metadata
are stored on the candidate row.
Reflection is a future *producer* of this candidate shape, not a parallel
pipeline. `review session --propose` bootstrap is the first non-reflection
consumer.

The boundary is deliberately deterministic: ee stores and validates structured
candidate outputs that an external harness or deterministic producer provides.
It does not call an LLM, add an LLM SDK dependency, auto-apply derived
memories, or store private chain-of-thought. All mutation still flows through
`ee curate validate` and an explicit `ee curate apply`.

### Schema migration

Rebuild `curation_candidates` (standard SQLite 12-step table rebuild — the
existing `target_memory_id` `NOT NULL`/`REFERENCES` constraints cannot be
altered in place) to:

1. Extend `candidate_type` CHECK to include `'create_derived_memory'`.

2. Relax `target_memory_id` to nullable, gated by candidate_type:

   ```sql
   target_memory_id TEXT REFERENCES memories(id) ON DELETE CASCADE,
   CHECK (
     (candidate_type = 'create_derived_memory' AND target_memory_id IS NULL)
     OR (candidate_type != 'create_derived_memory' AND target_memory_id IS NOT NULL)
   )
   ```

   `target_memory_id` remains effectively `NOT NULL` for every existing
   mutation type. The create-object type must omit it because its identity is
   source-derived rather than target-mutating. This narrowness is load-bearing
   — broadcasting nullability across the whole table would force null-handling
   into every existing list/filter/apply path.

   The Rust storage shape changes with the schema: `CreateCurationCandidateInput`
   and `StoredCurationCandidate` must represent `target_memory_id` as
   `Option<String>`, and helper code should immediately call a
   `require_target_memory_id(candidate_type)`-style validator for all
   mutate-existing candidate types. Empty-string sentinels are retired.

3. Add `derivation_source_refs_json TEXT` (nullable), gated:

   ```sql
   derivation_source_refs_json TEXT,
   CHECK (
     (
       candidate_type = 'create_derived_memory'
       AND derivation_source_refs_json IS NOT NULL
       AND length(trim(derivation_source_refs_json)) > 0
       AND json_valid(derivation_source_refs_json)
     )
     OR (
       candidate_type != 'create_derived_memory'
       AND derivation_source_refs_json IS NULL
     )
   )
   ```

   This ADR keeps source-only derivation data exclusive to the new candidate
   type. If a later ADR wants source refs on rule/procedure/supersede
   candidates, it should deliberately reopen this CHECK instead of inheriting a
   broad nullable column by accident.

   Stored as a canonical, sorted, duplicate-free JSON array of typed source
   refs. Sort order is `(kind, id)` so the same source package has one byte
   representation:

   ```json
   [
     { "kind": "evidence_span", "id": "ev_...", "contentHash": "blake3:..." },
     { "kind": "memory", "id": "mem_...", "contentHash": "blake3:..." }
   ]
   ```

   FK-like integrity is enforced in code at insert and apply time. Every source
   ref must include a `contentHash`; otherwise source drift is not auditable.
   Memory refs must exist in `memories`, be in the candidate's workspace, not be
   tombstoned, and match a canonical BLAKE3 hash of current `StoredMemory.content`.
   Evidence-span refs must exist in `evidence_spans`, be in the candidate's
   workspace, be currently unlinked, and match `StoredEvidenceSpan.content_hash`.
   This is SQLite, not Postgres — there is no JSONB; the column lives as `TEXT`
   with canonical encoding.

4. Add `derivation_metadata_json TEXT`, gated:

   ```sql
   derivation_metadata_json TEXT,
   CHECK (
     (
       candidate_type = 'create_derived_memory'
       AND derivation_metadata_json IS NOT NULL
       AND length(trim(derivation_metadata_json)) > 0
       AND json_valid(derivation_metadata_json)
     )
     OR (
       candidate_type != 'create_derived_memory'
       AND (
         derivation_metadata_json IS NULL
         OR json_valid(derivation_metadata_json)
       )
     )
   )
   ```

   The column is required for `create_derived_memory` because it carries the
   common `memorySpec` needed to create the memory. It remains optional for
   existing candidate types so future producers can attach metadata to other
   mutation shapes without another table rebuild.

   The value is a structured envelope with a stable common section plus a
   producer-specific payload:

   ```json
   {
     "schema": "ee.curation.derivation_metadata.v1",
     "memorySpec": {
       "level": "semantic",
       "kind": "rule",
       "tags": ["cass-review", "derived"],
       "confidence": null,
       "utility": null,
       "importance": null,
       "validFrom": null,
       "validTo": null
     },
     "producer": { "surface": "review_session", "kind": "deterministic" },
     "producerPayload": {}
   }
   ```

   `curate apply` reads only the common `memorySpec` fields needed to create the
   memory. `CreateMemoryInput` requires concrete `confidence`, `utility`, and
   `importance` values, so null scores resolve deterministically: confidence =
   `candidate.proposed_confidence.unwrap_or(candidate.confidence)` clamped to
   `UnitScore`, falling back to `TrustClass::AgentAssertion.initial_confidence()`
   only when the candidate has no usable score; utility and importance fall back
   to `UnitScore::neutral()` unless the producer supplies bounded scores.
   Producer-specific details (`reflection_kind`, `request_id`,
   `prompt_template_hash`, etc.) live under `producerPayload` and are owned by
   the producer-side ADR.

5. Preserve the existing candidate-type CHECK expansion:

   ```sql
   CHECK (
     candidate_type IN (
       'consolidate',
       'promote',
       'deprecate',
       'supersede',
       'tombstone',
       'merge',
       'paraphrase_dedup_proposal',
       'split',
       'retract',
       'rule',
       'anti_pattern_proposal',
       'procedure',
       'create_derived_memory'
     )
   )
   ```

   The migration must not accidentally drop any existing candidate type while
   adding the new one. It should also reconcile the live enum/DB drift by
   preserving `CandidateType::ParaphraseDedupProposal` in the rebuilt CHECK;
   V060's table text currently omits it even though the Rust enum accepts it.

The rebuild follows the existing `curation_candidates` rebuild pattern used by
V030/V034/V060, not an `ADD COLUMN`-only migration. Indexes on
`curation_candidates` are recreated as part of the rebuild. Current schema
inspection shows no child table with `REFERENCES curation_candidates(id)`; if
one is added before this ADR lands, the migration must explicitly account for
that child FK rather than assuming a rename/rebuild preserves it.

### Validation path

`ee curate validate <candidate-id>` must dispatch by `candidate_type` before it
loads a target memory. The live code currently loads `stored.target_memory_id`
up front in `validate_curation_candidate`; that is correct for mutate-existing
candidates and wrong for `create_derived_memory`.

`validate_create_derived_memory_candidate` validates the source refs, metadata
envelope, proposed content, scope, duplicate risk, and policy checks without a
target memory. Existing target-memory validation remains required for every
other candidate type.

### Apply path (one atomic transaction)

`apply_create_derived_memory_candidate` runs every step inside a single DB
transaction. Use the write-owner-protected transaction helper
(`DbConnection::with_transaction`) or an equivalent current-transaction helper;
do not call wrappers such as `insert_memory_audited` that open their own nested
transaction inside the apply transaction.

0. Dispatch by `candidate_type` before loading a target memory. Existing apply
   arms still require a target; `create_derived_memory` is the only branch that
   starts from source refs instead.

1. Validate every entry in `derivation_source_refs_json`. Memory refs must
   exist in `memories`, be in the candidate's workspace, not be tombstoned, and
   match the supplied hash. Evidence-span refs must exist in `evidence_spans`,
   be in the candidate's workspace, match the supplied hash, and be unlinked.
   Reject as `derived_sources_invalid` (severity `high`) otherwise.

2. Re-run the same prompt-injection and secret/redaction policy semantics used
   by memory creation on `proposed_content`. The implementation should share or
   extract the existing helper instead of creating a parallel guard.

3. Insert the new memory with `trust_class = AgentAssertion`,
   level/kind/tags/validity from `derivation_metadata_json.memorySpec`, and
   scores resolved by the `memorySpec` fallback rules above (`AgentAssertion`'s
   class default is defined at `src/models/trust.rs:92`). `provenance_uri` must
   either use one of the existing parsed schemes from `src/models/provenance.rs`
   (`cass-session://`, `file://`, `ee-mem://`, `http(s)://`, `agent-mail://`) or
   stay null; do not invent a `curation-candidate://` scheme in this slice. The
   candidate id and full source package live in the audit details.

4. Insert `MemoryLinkRelation::DerivedFrom` links from the new memory to each
   memory source ref. `DerivedFrom` already exists at `src/db/mod.rs:14157`; no
   new relation variant.

5. Attach evidence-span source refs to the new memory by setting
   `evidence_spans.memory_id` in the same transaction. This only applies to
   unlinked spans; spans already linked to a different memory reject the apply
   as `derived_sources_invalid`.

6. Enqueue a search-index job for the new memory.

7. Write a normal `memory.create` audit row targeted at the new memory, with
   `details.schema = "ee.audit.derived_memory_created.v1"`,
   `sourceAction = "curation_candidate.apply"`, the candidate id, source refs,
   and producer identity when present. `ee.audit.derived_memory_created.v1` is a
   details schema, not a new global audit action. Producer identity goes into
   the audit row's `details` JSON, not as a new column on the global audit
   table.

8. Mark the candidate `Applied`.

All apply steps share the transaction. If any step fails the candidate keeps its
pre-apply state (normally `Approved`) and the surface returns a structured
failure reason. The atomicity test enforces this — no orphan memory row, no
half-written link set, and no partial candidate status transition.

### Validators

Code-level validation runs before insert; the DB CHECK is the second line of
defense.

- `target_memory_id IS NULL` is rejected for any `candidate_type !=
  'create_derived_memory'`. Better error message than the bare DB constraint
  rejection.
- `target_memory_id IS NOT NULL` is rejected for
  `candidate_type == 'create_derived_memory'`; source-derived creation must not
  smuggle in target-mutation semantics.
- `target_memory_id = ""` is rejected for every candidate type. The current
  bootstrap sentinel becomes invalid once this ADR lands.
- `derivation_source_refs_json` is required, parseable as a canonical sorted
  JSON array, non-empty, duplicate-free, and every typed ref resolves with a
  matching `contentHash` at insert time.
- `derivation_metadata_json.memorySpec` is required for
  `create_derived_memory`; its level/kind/tags/validity/scoring fields use the
  same parsers and bounds as `ee remember` / `UnitScore`.
- `proposed_content` runs through the existing redaction + prompt-injection
  guard. The producer-side ADR (reflection) layers further per-kind
  post-condition validators on top — those live outside this ADR.

### First non-reflection consumer

`ee review session --propose` bootstrap candidates become
`create_derived_memory` rows with `evidence_span` source refs. This matches the
live bootstrap path: the skipped candidates carry CASS evidence span IDs, not
memory IDs. The acceptance gate for this slice — that bootstrap candidates
persist and flow through `ee curate candidates` + `ee curate validate` +
`ee curate apply` — is met by the closed implementation (the prior inline
TODO in `src/core/curate.rs` has since been deleted; `CandidateType::CreateDerivedMemory`
is the live struct definition). bd-2d32o is referenced as the precursor in
the bead that implemented this slice.

The producer output changes too: `ee.review.session.v1` must stop emitting the
empty-string `targetMemoryId` sentinel for bootstrap candidates. Bootstrap
review candidates should serialize `candidateType = "create_derived_memory"`,
`candidateKind = "propose_new_memory"`, and `targetMemoryId = null`; the prior
candidate kind can also be copied into `derivation_metadata_json.producerPayload`
when persisted.

Reflection ingest (separate ADR / slice) becomes the second consumer,
populating `derivation_metadata_json` with `reflection_kind`, `producer`,
`request_id`, and `prompt_template_hash`.

## Alternatives considered

**Per-reflection-kind candidate variants (`reflection_summary`,
`reflection_insight`, …).** Rejected. Eight new closed-enum variants, eight
CHECK migrations, eight validator arms, eight apply arms. Conflates
*derivation semantics* (what the LLM was asked to produce) with *mutation
shape* (what the database change is). Reflection kind is properly a property
of the request artifact, not the candidate row.

**Make `target_memory_id` nullable for every candidate type.** Rejected. High
blast radius — every list/filter/apply path assumes a target exists.
Broadcasting nullability invites null-handling bugs in unrelated code. The
gating CHECK contains the change to just the new type.

**Materialize a placeholder memory at candidate creation time.** Rejected.
The placeholder has no provenance, no content, needs its own tombstone
lifecycle. Inverts normal flow (memory comes from applying the candidate, not
the other way around). Pollutes the memory table with empty rows.

**Sibling `derived_memory_candidates` table FK-linked to
`curation_candidates`.** Rejected. Splits state across two tables, doubles
read paths, doesn't actually solve the `target_memory_id NOT NULL` problem —
the parent row still needs a target or the FK still needs relaxing. It also
does not cover evidence-span-sourced bootstrap candidates unless it invents a
second source-reference model.

**Producer identity as a column on the global `audit` table.** Rejected (per
peer review). Existing audit rows use a `details` JSON column. If producer
attribution ever needs indexed query, the column belongs on a derived-
candidate or reflection-specific table, not on the global audit table.

**Store raw chain-of-thought as `proposed_content`.** Rejected.
`src/models/recorder.rs:605` explicitly defines visible rationales as "not
raw private model chain-of-thought, scratchpads, or complete hidden
transcripts." `proposed_content` is the distilled output. A reflection-result
post-condition validator (later slice) will reject CoT-shaped bodies.

**Use `MemoryLinkRelation::Supports` for the source links.** Rejected.
`DerivedFrom` is semantically correct and already exists. `Supports` is for
"this memory provides evidence that the target is true," which is a different
relationship.

## Consequences

**Schema migration is real but contained.** One table rebuild, no cross-table
cascade. Existing rows migrate unchanged — their non-null `target_memory_id`
satisfies the new CHECK. Indexes are recreated as part of the rebuild; if
`curation_candidates` triggers are added before this ADR lands, the
implementation must recreate them too.

**Code surface additions are contained.** One
`CandidateType::CreateDerivedMemory` variant, one
`validate_create_derived_memory_candidate` function, one
`apply_create_derived_memory_candidate` function, one validator extension, one
evidence-span attachment helper, one audit-details schema
(`ee.audit.derived_memory_created.v1`), and JSON contract extensions on
`ee.curate.candidates.v1`, `ee.curate.validate.v1`, and `ee.curate.apply.v1`
to surface `derivation_source_refs`, `derivation_metadata`, and created-memory
fields when present.

**Nullable target becomes an explicit API contract.** `targetMemoryId` is
`null` when `candidateType == "create_derived_memory"` and non-null for every
other candidate type everywhere curation candidates are serialized, including
`curate candidates`, `curate validate`, and `curate apply`. `curate apply` must
not overload `targetMemoryId` with the newly created memory id; it should expose
that id through a distinct `createdMemoryId` / `createdMemory` field and leave
target before/after state null for this candidate type. JSON schema and golden
fixtures must pin that shape so clients do not continue relying on the old
empty-string bootstrap sentinel.

**Target-indexed curation helpers need explicit null behavior.** Live curation
code groups duplicates and computes structural decay from `target_memory_id`
before validation/apply. Those helpers must not unwrap or synthesize an empty
target for `create_derived_memory`: duplicate grouping should use the canonical
derived content/source-ref package key, and structural decay should use memory
source refs when present or a neutral/no-structural adjustment for evidence-only
bootstrap candidates.

**`review session --propose` becomes meaningfully complete.** Bootstrap
candidates persist and apply through the normal path. bd-2d32o's deferral
lifts.

**Foundation for reflection.** The reflection-handshake protocol (later
slice) becomes a thin producer layer over the same candidate shape.

**Closure-lint taxonomy.** Ships as
`implements-surface:external_derivation_candidates`. Acceptance: a sentinel
search confirms `create_derived_memory` lands in the enum + DB CHECK, the e2e
test passes, the migration applied to a fresh DB produces the expected
schema, and the contract test for the extended candidate envelope passes.

## Verification hooks

- **Schema contract test** (`tests/contracts/curation_candidates_schema_v2.rs`):
  rebuilt table has the new CHECK constraints; the IN list contains
  `create_derived_memory`; `target_memory_id` is null exactly for
  `create_derived_memory`; new JSON columns present and guarded by `json_valid`.
- **Response contract test**: `ee.curate.candidates.v1`,
  `ee.curate.validate.v1`, `ee.curate.apply.v1`, and `ee.review.session.v1`
  all serialize `targetMemoryId: null` for `create_derived_memory` bootstrap
  candidates; `curate apply` surfaces the new id as `createdMemoryId`, keeps
  target before/after null, and never returns the created memory id as
  `targetMemoryId`.
- **Migration determinism test**: applying the migration to a fresh DB twice
  yields byte-identical schema.
- **Migration preservation test**: applying the migration to a non-empty DB
  preserves existing curation candidates across all pre-existing candidate
  types, keeps review/TTL fields intact, and rewrites the CHECK list without
  losing `paraphrase_dedup_proposal`.
- **E2E test** (`tests/e2e_derived_memory_candidate.rs`): `ee review session
  <id> --propose` against a fixture CASS corpus → bootstrap candidate persists
  → `ee curate validate <id>` approves it without loading a target memory →
  `ee curate apply <id>` creates a new memory, attaches the source evidence spans
  to that memory, writes a `memory.create` audit row with
  `details.schema = "ee.audit.derived_memory_created.v1"`, and enqueues a
  search-index job. The derived content/source-ref package hash is
  deterministic; generated memory ids and timestamps are normalized in the
  assertion.
- **Memory-source unit test**: a `create_derived_memory` candidate with memory
  source refs creates N `DerivedFrom` links and no evidence-span attachment.
- **List/sort unit tests**: duplicate grouping, TTL/structural decay adjustment,
  and `--target` filtering handle `target_memory_id = NULL` without falling back
  to the retired empty-string sentinel.
- **Candidate type contract test**: `CandidateType::all`, `as_str`, `FromStr`,
  `requires_content`, the parse-error expected-list message, and the DB CHECK
  all include `create_derived_memory` exactly once and keep
  `paraphrase_dedup_proposal`.
- **Provenance URI contract test**: derived-memory creation never writes an
  unregistered provenance URI scheme; candidate provenance remains available
  through audit details and source refs.
- **Failure-mode fixtures** under `tests/fixtures/failure_modes/`:
  - `derived_sources_invalid.json` — source ref missing / duplicate / missing
    `contentHash` / cross-workspace / tombstoned memory / evidence span already
    linked elsewhere / content-hash mismatch.
  - `derived_target_required_for_mutation.json` — null target on a
    non-`create_derived_memory` candidate.
  - `derived_target_forbidden_for_create.json` — non-null target on a
    `create_derived_memory` candidate.
- **Validator unit tests**: empty source list, duplicate source refs, missing
  `contentHash`, source from another workspace, tombstoned memory source,
  evidence span already linked elsewhere, malformed `derivation_source_refs_json`,
  missing `memorySpec`, and non-null target on `create_derived_memory` each
  rejected with stable degraded codes.
- **Atomicity test**: simulated failure during apply (search-index enqueue
  forced to error) leaves the candidate in its pre-apply state and leaves no
  orphan memory, link, evidence-attachment, audit, or search-index rows.

## Open questions deferred

- Producer identity normalization for SPRT quarantine (harmful feedback on
  derived memories attributing back to producer agent identity). Defer to
  the reflection-handshake ADR — producers are a first-class concept there.
- Auto-apply policy for high-confidence derived candidates. Defer; every
  `create_derived_memory` requires explicit `ee curate apply` for now.
- Nested derivation (sources include a memory that was itself derived).
  Allowed for memory source refs; the `DerivedFrom` graph captures the chain.
  Evidence-span provenance remains attached through `evidence_spans.memory_id`
  and audit details. Reflection-side post-condition validators may bound depth
  to prevent reflection-on-reflection laundering.

## References

- bd-2d32o (closed) — bootstrap candidate FK blocker; precursor.
- The bootstrap-candidate inline TODO this ADR retired has been deleted from
  `src/core/curate.rs`; `CandidateType::CreateDerivedMemory` is the live
  enum variant the ADR introduced.
- `src/curate/mod.rs:363` — `CandidateType` enum extended by this ADR.
- `src/db/mod.rs:5133` — `curation_candidates` target FK rebuilt by this ADR.
- `src/db/mod.rs:14157` — `MemoryLinkRelation::DerivedFrom` used unchanged.
- `src/db/mod.rs:3702`, `src/db/mod.rs:3926`, `src/db/mod.rs:5120` —
  existing `curation_candidates` table rebuild migrations this ADR follows.
- `src/models/recorder.rs:605` — visible-rationale policy; bounds
  `proposed_content` content shape.
- `src/models/trust.rs:92` — `TrustClass::AgentAssertion` initial confidence
  for derived memories.
- `src/models/provenance.rs:9` — accepted provenance URI schemes; this ADR does
  not add a curation-candidate scheme.
- `src/core/handoff.rs:1608` — HMAC `derive_key` precedent for any future
  signed-derivation extension.
