# ADR-0019: Learning Clusters And Uncertainty

## Status

Accepted

## Context

`ee learn` must turn persisted evidence into procedural-memory work without depending on an LLM, a daemon, or hard-coded fixture templates. The inputs available in the walking CLI are memories, memory tags, feedback events, curation candidates, and explicit learning observations. The output must be deterministic so golden tests can freeze agenda, uncertainty, summary, and proposal contracts.

## Decision

Learning clusters are keyed by normalized topic. For feedback linked to a memory, the topic is the first stable memory tag, falling back to memory kind and then target type. Evidence pointers are the feedback event ID, target ID, source ID, and evidence IDs embedded in observation/outcome JSON.

Uncertainty is the maximum of two deterministic signals:

- normalized Shannon entropy over positive, negative, and neutral/decay outcome buckets
- scarcity pressure for clusters with fewer than four observations

Confidence is the positive-plus-half-neutral share of total weighted evidence. Proposals are emitted only for non-trivial clusters with at least two evidence pointers and a target memory, then persisted as `rule` curation candidates with deterministic IDs.

## Consequences

The metric favors contradictory evidence and under-sampled topics for agenda work, while still allowing large consistent clusters to propose candidate procedural rules. The algorithm is simple enough to audit from JSON output and cheap enough to run synchronously inside the CLI.

This deliberately does not implement semantic embeddings in the learning pass. Tag co-occurrence is the deterministic phase-0 approximation; semantic similarity can be added later through Frankensearch-derived topic expansion without changing the persisted evidence contract.

## Verification

- Empty ledger: reports no gaps and no proposals.
- Single observation: reports a scarce open question with sample IDs.
- Contradictory observations: reports high entropy and lower confidence.
- Large cluster: emits deterministic experiment/candidate IDs and persists one curation candidate.
