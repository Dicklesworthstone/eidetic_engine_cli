# ADR 0082: Code-anchor drift is a FLAG, not a rank penalty

Status: accepted
Date: 2026-06-17
Bead: bd-2vq2z.1
Supersedes: ADR 0056 part B (the rank-down freshness penalty)

## Context

ADR 0056 ("Code-anchoring substrate") part B introduced **code-coupled
freshness**: a memory anchored to a code surface (path/symbol) captures a
blake3 span fingerprint at anchor time, a steward job recomputes it against
git-changed files, and drift writes an audited `memory.freshness_transition`
row. To act on that signal, part B multiplied the freshness term in
`src/search/scoring.rs` by a **rank-down penalty** (a drifted anchor fell in
rank, clamped to a floor of `0.4` so it never vanished). That penalty was
applied unconditionally in two live places:

- `src/search/scoring.rs::SearchScoreComponents::from_signals` (the shared
  retrieval-multiplier contract), and
- `src/core/recall.rs::score_row` (the `ee recall` ranking path),

both hard-coding `DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR = 0.4`.

The owner's 2026-06-17 review (bd-2vq2z.1, Phase-6 pass 2) judged this the
**wrong default**. A memory anchored to code that *just changed* is very often
*exactly* what an agent wants when it touches that code again — penalizing its
rank hides the most relevant memory at the worst moment. The existential value
of the drift signal is **trust** ("this references code that changed —
re-verify"), and trust is served by a visible flag, not by suppression.

## Decision

**Code-anchor drift is surfaced as a FLAG, and by default does NOT reduce rank.**

1. **Default behavior is flag-only.** The freshness-drift multiplier is now
   driven by a configurable `stale_anchor_penalty` whose default is `0.0`.
   A penalty of `0.0` maps (via `scoring::stale_anchor_floor`) to a floor of
   `1.0`, i.e. a neutral `1.0` multiplier for every freshness state — a drifted
   memory keeps its rank. The drift remains visible through the existing
   freshness flag (`freshness_state: suspect | stale` on recall items; the
   audited `memory.freshness_transition` row; `ee memory drift`).

2. **Penalty is opt-in and a tie-breaker at most.** `stale_anchor_penalty`
   lives in the retrieval contract (`SearchScoringConfig.stale_anchor_penalty`,
   `RecallQuery.stale_anchor_penalty`) and is intended to be resolved from
   `[retrieval] stale_anchor_penalty` in workspace config. A penalty `p` in
   `(0.0, 1.0]` lets a `Stale` anchor fall at most to `1.0 - p` and a `Suspect`
   anchor to the midpoint; the multiplier never reaches zero, so a drifted
   memory never vanishes. `freshness_drift_multiplier(state, floor)` itself is
   unchanged — only the *floor it is given* now comes from the configurable
   penalty (`floor = 1.0 - penalty`).

3. **Determinism is preserved.** The pack/recall flag derives from the
   **persisted** `memory_anchors.freshness_state` (deterministic given the DB),
   not from a live git/hash recompute at ranking time. The live recompute stays
   confined to the explicit, budget-respecting `ee memory drift` /
   `ee diag memory-drift` surface and to the audited steward freshness job, so
   "same DB + config + query → byte-identical output" still holds.

4. **Confidence decay stays separate.** Decaying the confidence of a
   long-drifted, never-revalidated anchored memory remains a distinct, audited
   steward action (`memory.freshness_transition` / curation path), never a
   retrieval-time penalty.

## Consequences

- The default `ee recall` and shared retrieval ranking no longer rank drifted
  memories down; they surface the drift flag instead. Existing
  `freshness_drift_multiplier` semantics and tests are unchanged (a floor of
  `1.0` is the neutral case); only the *default floor in use* moved from `0.4`
  to `1.0`.
- Operators who want the old behavior can set `[retrieval] stale_anchor_penalty
  = 0.6` (≈ the previous `0.4` floor).
- The shared posture is: **surface drift, do not suppress relevant memories.**

## Verification hooks

- `src/search/scoring.rs`:
  `from_signals_flags_drift_without_penalizing_by_default`,
  `from_signals_applies_opt_in_stale_anchor_penalty`,
  `stale_anchor_floor_maps_penalty_to_clamped_floor`.
- `src/core/recall.rs`:
  `drifted_anchor_is_flagged_not_penalized_by_default`,
  `opt_in_stale_anchor_penalty_ranks_drift_down_without_vanishing`.
- Remaining leaf step (tracked under bd-2vq2z.1): wire
  `[retrieval] stale_anchor_penalty` through `src/config` so the recall handler
  resolves an operator override instead of always using the `0.0` default.
