# ADR 0022: Causal Evidence Ledger

Status: accepted
Date: 2026-05-06
Bead: eidetic_engine_cli-hnrm

## Context

The causal command surface needs to explain failure causes without inventing
chains. Earlier degraded-mode behavior correctly refused to report generated
causal traces, estimates, comparisons, or promotion plans because there was no
durable evidence ledger behind the output. That honesty contract should remain,
but the real surface now needs a persisted source that can support deterministic
causal projections.

## Decision

Add a `causal_evidence` ledger table as the durable source for causal edges.
Each row records a directed relationship from a failure memory to a candidate
cause memory, a bounded contribution score, evidence URIs, computation time,
and a method from the fixed set `manual`, `graph-inferred`, or `cass-derived`.

`ee causal trace <failure-mem-id>` projects ledger edges into deterministic
chains. The trace is depth-limited, sorted stably, and rejects cycles instead of
masking them. Empty or missing evidence returns an explicit empty report with
degradation metadata, never synthetic chains.

`ee causal estimate <chain-id>` computes deterministic contribution estimates
from the stored edge scores. `ee causal compare <chain-a> <chain-b>` compares
those estimates without introducing a custom ranking model. `ee causal
promote-plan <chain-id>` creates a curation candidate with the chain evidence
attached; it does not automatically promote a rule or mutate the original
memories.

## Consequences

Causal output now has a database-backed contract. Every chain can show which
failure memory, cause memory, score, method, and evidence URI supported it.
Search, graph, and CASS import paths can add ledger rows later without changing
the CLI contract.

The first promotion target is a curation `procedure` candidate because the
existing curation type taxonomy does not yet include a distinct plan-recipe
candidate type. A future plan-recipe adapter can consume the same chain evidence
without changing the ledger schema.

## Rejected Alternatives

- Generate causal chains directly from command arguments.
- Treat graph centrality or search rank as causal proof.
- Store causal evidence only in JSON command artifacts.
- Promote plans automatically without a curation candidate and review step.

## Verification

- The migration stream creates `causal_evidence` with foreign keys, score
  bounds, JSON evidence validation, method validation, and stable indexes.
- Core tests cover empty evidence, single and branching chains, deterministic
  estimates, promotion candidate creation, and cycle rejection.
- Contract tests seed real memories and ledger rows, then exercise `trace`,
  `estimate`, `compare`, and `promote-plan` through the CLI JSON surface.
- Degraded-honesty tests prove the trace command reports empty evidence queries
  without generated chains.
