# ADR 0014: Propose-Validate-Apply Curation Flow

Status: accepted
Date: 2026-05-05

## Context

Memory mutations (consolidation, promotion, deprecation, tombstoning) are
high-stakes operations. A bad merge loses information; a premature promotion
elevates unvalidated claims. ADR 0006 requires procedural memory to have
evidence, but does not specify the state machine for mutation proposals.

Without a formal flow, mutations happen immediately and cannot be audited,
undone, or batched for human review.

## Decision

**All memory mutations go through a three-phase curation queue:**

### Phase 1: Propose

A `CurationCandidate` is created with:
- `candidate_type`: consolidate, promote, deprecate, supersede, tombstone, merge,
  split, retract, or rule
- `target_memory_id`: the memory being mutated
- `proposed_*` fields: the mutation payload
- `source`: agent_inference, rule_engine, human_request, feedback_event,
  contradiction_detected, decay_trigger, or counterfactual_replay
- `confidence`: 0.0–1.0 score from the proposer
- `status`: starts as `pending`

The candidate is persisted but the target memory is unchanged.

### Phase 2: Validate

Validation can be:
- **Automatic**: rule engine checks confidence threshold, contradiction absence,
  specificity score (ADR 0010).
- **Human**: `ee curate review` surfaces pending candidates for approval/rejection.
- **Agent**: external agent reads candidates via `ee curate list --pending --json`
  and calls `ee curate approve <id>` or `ee curate reject <id>`.

Status transitions:
- `pending` → `approved` (validation passed)
- `pending` → `rejected` (validation failed)
- `pending` → `expired` (TTL elapsed without decision)

### Phase 3: Apply

`ee curate apply <id>` takes an `approved` candidate and executes the mutation:
- Updates the target memory or creates derived records.
- Writes an audit record linking candidate to mutation.
- Transitions status to `applied`.

Only `approved` candidates can be applied; attempting to apply `pending`,
`rejected`, or `expired` candidates returns an error.

## Consequences

- No silent memory mutation; every change has an auditable candidate record.
- Human review is possible before high-stakes mutations.
- Agents can batch proposals and apply them after validation.
- Rollback is conceptual: create a `retract` candidate for the prior mutation.
- Queue depth is visible via `ee curate stats --json`.

## Native extractive episodic summaries

`ee curate candidates --type summary` previews deterministic proposals without
writing. Adding `--propose` persists pending `create_derived_memory` candidates
whose producer is `episodic_summary`. The normal `curate validate` and `curate
apply` commands remain separate. `--target-memory` requires that memory to be a
source member; it never identifies a row to overwrite. Existing candidate
status, sorting and pagination options continue to apply. Rejected and applied
proposal identities are not rediscovered as new pending work.

Discovery reads at most 128 current episodic rows in ID order. Targeted discovery
puts the requested source first and limits its neighborhood to the same kind and
workflow. This is a bounded source window, not an exhaustive similarity search.
A further row, a source larger than 16 KiB, or a cluster exceeding the 16-source
proposal limit emits `curate_summary_source_limit`; an empty result with that
warning does not prove no summary can be made. Large clusters contribute a
deterministic subset, retaining the requested target when present.

The proposer groups textual `fact`, `decision`, `observation`, `note` and
`summary` sources only when their workflow, exact scope tags and source authority
scope agree. At least three distinct bodies are required; duplicated captures
cannot establish repeated evidence. Typed payloads, explicit anchors, stale
anchors and sentinel-bearing memories are excluded. Automatically extracted
anchors must still appear verbatim in the selected source span. Closed seals,
pending review holds, quarantined sources, superseded revisions, tombstones,
future or expired applicability and malformed bounds all prevent admission.
Secret-like source content is excluded before extraction.

Topical grouping reuses the existing deterministic average-linkage cluster
engine with 256-dimensional hash embeddings, a 0.78 merge threshold and the
existing 0.40 silhouette gate. These are lexical topical-coherence measurements,
not semantic-model confidence or independent corroboration. The proposer
abstains on stored contradiction/supersession links within the source cluster
and on the conservative textual opposition already recognized by `ask`, including
negation, incompatible settings and reversed command order. Undetected or
paraphrased opposition remains a reason for explicit review.

Each proposal includes one distinct whole source span per cited memory, selected
using the existing extractive span segmenter and scorer. Fences and command
clauses remain whole; no sentence is byte-truncated or paraphrased. Each span
carries its source ID, exact UTF-8 byte range, excerpt hash and a commitment to
the source row and authority/scope sidecars in the versioned
`ee.curate.episodic_summary.v1` producer payload. If every cited source cannot be
represented within 2 KiB per span and an 8 KiB output, the proposer abstains.
This selects observations; it does not assert that every source detail is in
the summary.

For this native producer only, exact validated source coverage replaces the
generic rule-prose specificity heuristic. Ordinary observations need not name
a command or path merely to pass that heuristic. Prompt-injection, trust,
content and metadata checks remain mandatory, and manual/external derived
candidates retain the existing specificity gate. A producer label alone cannot
authorize the exception: reconstruction must match the exact committed native
proposal and all live source state.

The new memory is semantic `summary` with `agent_assertion` trust and confidence
no greater than either 0.60 or the weakest source. It retains the common tags,
workflow and applicable agent-producer URI, and intersects the source validity
windows. Validation reconstructs the exact extractive proposal from live
sources. Apply repeats that reconstruction inside the serialized writer
transaction before inserting anything; changed source bodies, scores, scope,
holds, validity, revisions or detected conflicts invalidate the old proposal.
The existing derived-memory apply path creates provenance links, audit records
and index work. Original source rows remain intact, and replay converges on the
same derived memory rather than duplicating it. Cancellation rolls back a
proposal batch and never authorizes a derived write.

## Rejected Alternatives

- **Immediate mutation with undo log**: Harder to surface for review before commit.
- **Two-phase (propose/apply) without validation**: Skips the review step.
- **External workflow engine**: Adds dependency; the queue belongs in `ee`.

## Verification

- `src/curate/mod.rs`: `CandidateStatus` enum enforces the state machine.
- `tests/curate_state_machine.rs`: Asserts valid/invalid transitions.
- Golden tests for `ee curate list --pending --json` output shape.
- E2E test: propose → approve → apply → verify memory changed.
- `src/core/curate_summary_tests.rs`: native preview → proposal → validation →
  apply, exact source coverage and lineage, original-row preservation, replay,
  contradictory and restricted sources, source changes inside the writer
  boundary, bounded discovery and request cancellation.
