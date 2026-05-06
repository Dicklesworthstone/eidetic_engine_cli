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

## Rejected Alternatives

- **Immediate mutation with undo log**: Harder to surface for review before commit.
- **Two-phase (propose/apply) without validation**: Skips the review step.
- **External workflow engine**: Adds dependency; the queue belongs in `ee`.

## Verification

- `src/curate/mod.rs`: `CandidateStatus` enum enforces the state machine.
- `tests/curate_state_machine.rs`: Asserts valid/invalid transitions.
- Golden tests for `ee curate list --pending --json` output shape.
- E2E test: propose → approve → apply → verify memory changed.
