# ADR 0044: High-Risk Reflection Routing

Status: proposed
Date: 2026-05-23
Bead: bd-1bd0f

## Context

The reflection handshake roadmap separates low-risk derived-memory kinds
(`summary`, `insight`, `gaps`, `strengths`, `question`, and `plan`) from two
higher-risk kinds: `procedural_extract` and `contradiction_resolve`.

Those two kinds can affect future agent behavior more directly than a derived
summary or gap:

- `procedural_extract` can produce instructions, rules, or procedures that an
  agent may later follow.
- `contradiction_resolve` can decide how conflicting memories should be
  interpreted, deprecated, superseded, or retained.

ADR 0014 already requires high-stakes memory mutations to go through
propose-validate-apply. ADR 0006 requires procedural memories to carry
evidence. The external-derivation roadmap adds a generic
`create_derived_memory` candidate shape, but that shape is not a license for an
external producer to auto-promote rules or settle contradictions.

The design question is how these high-risk reflection kinds should enter the
curation queue without adding an LLM dependency, a parallel reflection table, or
source-reference columns to every target-mutating candidate type before there is
implementation evidence that such generalization is needed.

## Decision

High-risk reflection kinds are routed through a two-stage curation model.

### Stage 1: create an advisory derived memory

`procedural_extract` and `contradiction_resolve` reflection results initially
create `create_derived_memory` candidates only. The created memory remains
`trust_class = AgentAssertion` and must use a non-procedural initial level unless
a later ADR explicitly raises that bar with stronger validation evidence.

The derived memory records the distilled claim and its source package:

- `procedural_extract` creates a scoped, testable procedural draft. The memory
  body may describe a rule or procedure candidate, but it is not yet a
  high-confidence procedural memory.
- `contradiction_resolve` creates a resolution draft that names the conflicting
  sources, the proposed interpretation, and confidence limits. It does not
  supersede, tombstone, or merge source memories.

This stage is safe because it preserves the useful result as evidence while
requiring normal curation before any stronger mutation happens.

### Stage 2: propose an explicit target mutation

Promotion or mutation happens through a separate curation candidate after the
advisory memory exists and has been validated:

- A procedural draft can later produce an ordinary `rule`, `procedure`, or
  promotion candidate if it passes the procedural-memory evidence rules.
- A contradiction-resolution draft can later produce an ordinary `supersede`,
  `retract`, `merge`, or link-oriented candidate if the system has enough
  source evidence and an explicit target.

Those second-stage candidates must cite the advisory derived memory and the
original source ids in their audit details. They must not silently inherit trust
from the reflection result. They remain subject to the usual approval and apply
rules.

### Source-reference scope

`derivation_source_refs_json` remains exclusive to `create_derived_memory` in
this design. Existing target-mutating candidate types do not gain source-ref
columns in this slice.

If later implementation proves that target-mutating candidates need first-class
source refs, that must be a separate migration and ADR amendment. The amendment
must define the table constraints, canonical encoding, validation order, audit
shape, and backwards-compatible treatment of existing candidates.

### Reflection contracts

`ee.reflect.result.v1` keeps `reflectionKind` separate from
`candidateType`. The reflection kind explains producer intent; the candidate
type explains the storage mutation shape.

For high-risk kinds, valid results include kind-specific fields:

- `procedural_extract`: `sourceIds`, `scope`, `procedureDraft`,
  `preconditions`, `negativeExamples`, `confidenceLimits`, and optional
  `suggestedValidation`.
- `contradiction_resolve`: `conflictingSourceIds`, `resolutionMode`
  (`prefer_source`, `synthesize`, `insufficient_evidence`, or `defer`),
  `resolutionDraft`, `winningSourceIds`, `confidenceLimits`, and
  `openQuestions`.

The self-reported confidence from an external producer is informational only.
It can lower urgency or force review, but it cannot promote trust class, bypass
validation, or choose an apply path.

## Validators

### `procedural_extract`

A valid procedural extraction must:

- cite at least two independent source ids when possible;
- explain when a single-source extraction is unavoidable;
- produce imperative or otherwise testable guidance;
- state scope limits and preconditions;
- include at least one negative example, exception, or disqualifier;
- avoid turning one incident into a universal rule;
- pass prompt-injection and secret redaction policy;
- enter the queue as an advisory derived memory, not as an auto-promoted rule.

The validator rejects vague advice, unsupported best-practice claims,
overbroad language such as "always" without source support, hallucinated source
ids, and any result that asks the harness to apply the rule directly.

### `contradiction_resolve`

A valid contradiction resolution must:

- identify the conflicting source ids;
- identify whether the result prefers one source, synthesizes a narrower claim,
  marks the conflict unresolved, or defers for human review;
- cite the source ids that support the proposed resolution;
- preserve confidence limits and open questions;
- explain what would make the resolution stale;
- avoid tombstoning, superseding, or rewriting any source memory in the first
  stage.

The validator rejects results that omit conflicting sources, cite ids outside
the request package, claim certainty without evidence, or mutate source state
as part of ingest.

## Audit Model

Reflection propose and ingest events write reflection-specific audit details.
The candidate created by ingest writes the ordinary curation candidate audit
details plus:

- `reflectionKind`;
- `requestId` and `requestHash`;
- producer identity;
- cited source ids and content hashes;
- validator version;
- policy/redaction posture;
- a flag showing that the result is advisory and not auto-promoted.

If a later second-stage mutation is proposed, its audit details must link back
to both the advisory derived memory and the original source package. This keeps
the evidence chain visible in `ee why` without making the external producer an
authority.

## Rejected Alternatives

1. **Route `procedural_extract` directly to `rule` or `procedure`.** Rejected
   because an external producer would be able to create future agent guidance
   too quickly. Procedural memory needs validation and outcome evidence.
2. **Route `contradiction_resolve` directly to `supersede`.** Rejected because
   resolving conflicting memories can erase or demote useful evidence. The
   first durable artifact should be the proposed resolution and its limits.
3. **Generalize source refs to every candidate type now.** Rejected because the
   storage and validation surface is larger than the first high-risk routing
   decision requires.
4. **Create reflection-specific candidate types for each high-risk kind.**
   Rejected because `candidateType` should describe mutation shape, not producer
   intent. `reflectionKind` carries producer semantics.
5. **Auto-apply high-confidence reflection results.** Rejected because external
   confidence is not trust evidence in ee.

## Verification

Implementation of this ADR requires fixtures for:

- overbroad procedural extraction from one incident;
- procedural extraction with hallucinated source ids;
- contradiction resolution without conflicting source ids;
- contradiction resolution that cites ids outside the request package;
- any result that tries to auto-promote, supersede, tombstone, or apply during
  ingest.

Positive tests must prove that accepted high-risk results create pending
`create_derived_memory` candidates only, with `targetMemoryId = null`, retained
source hashes, `trust_class = AgentAssertion`, and no source-memory mutation.

RCH-backed tests are required when this design becomes code. Until then,
documentation verification is limited to Markdown lint/static checks and diff
review.
