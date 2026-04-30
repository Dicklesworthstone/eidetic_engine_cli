# ADR 0010: Deterministic Curation Specificity

## Status

Accepted.

## Context

The curation subsystem promotes, rejects, or rewrites memory through explicit
candidate records. The product principle is evidence over vibes: generic
procedural advice must not silently become durable guidance.

Before this ADR, "specific enough" had no executable definition. That leaves
`ee curate validate` and future auto-promotion paths open to implementation
drift.

## Decision

`ee` uses a pure deterministic specificity scorer for proposed curation content.
The scorer:

- extracts concrete tokens such as commands, file paths, error codes, metric
  thresholds, branch or tag names, provenance URIs, and technology names;
- reports generic tokens from a small embedded vocabulary;
- computes an explicit weighted sum with default threshold `0.45`;
- redacts concrete tokens that look credential-bearing before reporting them;
- fails candidates with stable reason `candidate_too_generic` when the score,
  concrete-token, structural-signal, or instruction-like checks do not pass.

The threshold is configurable as `[curation].specificity_min`. The weights are
kept in code for now so the v1 contract is deterministic and reviewable.

## Consequences

Future curation validation and auto-promotion paths can call one shared contract
instead of re-deciding specificity locally. The scorer intentionally does not
call an LLM and does not require network or model state.

The scorer is heuristic. It is allowed to reject borderline advice and require a
more concrete rule; that is cheaper than admitting durable procedural noise.

## Verification

- `src::curate` unit tests cover empty, generic, concrete, redacted,
  instruction-like, long, multilingual, and property-style cases.
- `tests/fixtures/specificity/` pins positive and negative fixture examples.
- Config parser and merge tests cover `[curation].specificity_min`.
