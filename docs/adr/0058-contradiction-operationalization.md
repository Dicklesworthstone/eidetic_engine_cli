# ADR 0058: Contradiction operationalization (detect → audited resolution + pack guard)

Status: proposed
Date: 2026-06-07
Bead: bd-1n0np.7.1

## Context

`ee` already **detects** contradiction structure (`graph/health.rs`
`detect_contradiction_clusters`, k-truss + Louvain, surfaced read-only via
`ee health --robot-insights`) but **nothing resolves it**. Two memories asserting
opposites can both keep ranking and land in the *same* pack — worse than having
neither, because it directly undermines trust in every pack. The 2026-06-07
review converged on this from all three models (scores 846/750/692): detection
without resolution is a dangling thread.

## Decision

Operationalize detection into the curation pipeline plus a pack guard.

- `ConflictCluster { members, kind: explicit_contradiction | temporal_supersession
  | duplicate_divergent | trust_split | stale_bridge | outcome_split, severity,
  confidence, load_bearing_evidence, recommended_actions }`; `ConflictPolicy =
  surface | deprioritize | fail_on_high | ignore`.
- **Explicit-evidence-first detection**: start from contradictions `ee` already
  knows (contradiction/supersession links, validity-window overlaps,
  duplicate-divergent, trust/outcome splits, repeated pack co-selection), reusing
  `health.rs` clusters ranked by centrality + load-bearing. Fuzzy near-conflict
  discovery (embedding opposition) is **deferred behind an opt-in flag** until its
  precision is proven.
- Surfaces: `ee conflict list/explain/cluster` (read-only) and `ee curate
  contradictions`. Resolutions route through the existing propose→validate→apply
  machinery (ADR 0014): supersede = tombstone-with-pointer, scope-split =
  tag/scope edit, merge = consolidation. **Never auto-supersede.**
- **Pack guard**: never include both sides of an *unresolved hard contradiction*
  in one pack; keep the higher-trust/fresher side and flag
  `contradiction_suppressed` (legible via `ee why-not`). Optional opt-in
  **forced-mode** surfaces both sides under a `## Contradictions` header, ranked
  and capped, for high-confidence load-bearing conflicts.

## Consequences

- **Easier**: self-contradicting packs become impossible by default; known
  conflicts are surfaced and resolvable with audit.
- **Guarded**: false positives are contained by explicit-evidence-first + the
  scope-split escape ("both true in different contexts") + human-confirmed
  resolution.
- **Intentionally impossible**: no auto-supersede; forced-mode is never the
  default (it spends scarce tokens).

## Rejected Alternatives

- **Lead with fuzzy embedding-opposition detection**: false-positive-prone;
  deferred behind a flag.
- **Auto-suppress/auto-resolve**: violates no-silent-mutation; rejected.
- **Forced-mode as default**: token cost + amplifies noisy detection; rejected
  for opt-in.

## Verification

- Unit + golden (bd-1n0np.7.6): detection per explicit kind; scope-split prevents
  false suppression; guard keeps higher-trust/fresher + flags
  `contradiction_suppressed`; forced-mode capped.
- e2e `scripts/e2e_contradiction.sh`: two opposed rules → `conflict list` →
  resolve (scope-split, supersede) → guard suppresses one side (confirmed via
  `ee why-not`) → capped forced-mode.
- Enriched by typed `supersedes` edges (ADR for typed kinds) and freshness
  (ADR 0056) for picking the surviving side.
