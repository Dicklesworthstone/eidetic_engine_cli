# Feedback-Gated Learning Layer

Once the Evidence Harvester (E2) supplies a stream of *outcomes* — did a packed
memory actually help? — `ee` can learn from it. The feedback-gated layer is three
**reporting-first**, conservative cores that turn that stream into honest signal
without ever silently mutating the store or overclaiming.

Bead lineage: `bd-1n0np.13` (feature), `13.1` (Token ROI Ledger), `13.2`
(Temporal Regime-Shift detection), `13.3` (Calibration-Honesty Report), `13.4`
(tests), `13.5` (e2e), `13.6` (docs/capabilities/help).

## Why "feedback-gated"

Every core here is **gated on a dense outcome stream** and stays quiet/abstaining
until one exists. On thin or cold-start data they report low confidence, never a
confident-looking guess — a sparse signal that reads as certainty is worse than
no signal. They reuse `wilson_interval` (conservative lower/upper bounds) and
`core::sprt` rather than opaque ML, and each pins a `blake3` `table_hash` for
determinism.

## The three cores (`src/core/outcome.rs`)

### Token ROI Ledger (`13.1`, `compute_token_roi`)

Utility-per-token: which packed buckets earn their scarce token cost?
- Per-bucket `(helpful, total, tokens)` aggregated by the caller along any
  dimension (memory / kind / section / lens / profile / task / anchor) — the core
  is dimension-agnostic.
- Ranks by **conservative** utility (`utility_per_1k_tokens` from the Wilson
  **lower** bound), so a few lucky hits on thin data cannot dominate the budget.
- Below `min_samples` a bucket is flagged `abstained`. Schema `ee.token_roi.v1`.
- **Reporting-only.** It informs how scarce pack tokens are allocated;
  Frankensearch still owns retrieval scores. A capped opt-in selection
  tie-breaker stays *out* until outcomes are dense (else it optimizes on priors
  and starves fresh evidence).

### Temporal Regime-Shift detection (`13.2`, `detect_regime_shift`)

A rule that was helpful for months can flip harmful after a toolchain/dependency
upgrade. SPRT (`core::sprt`) over a **trailing window** of recent outcomes
catches the flip without the stale helpful history masking it. It **only
proposes** a demotion curation candidate (`proposed_demotion`) when the recent
regime crosses the bad-source threshold — it never auto-demotes or mutates — and
stays quiet on thin windows.

### Calibration-Honesty Report (`13.3`, `calibration_honesty_report`)

The duel's sharpest self-correction: the original conformal-coverage pitch
overclaimed. Conformal's guarantee needs **exchangeability**, which a
coding-memory workload violates by construction (task families rotate, the
codebase evolves). So this is the honest reframe — per-situation-class
**empirical** hit-rate + explicit sample counts + **wide** Wilson intervals +
**loud** abstention (`n < 30` widens the interval to `[0, 1]` and sets
`abstained`). It deliberately uses **no** `guarantee`/`coverage` language. Schema
`ee.calibration_honesty.v1`.

## Two hard invariants

1. **Reporting-only, never silent mutation.** Token ROI informs budgeting,
   regime-shift only *proposes* a curation candidate, calibration only reports —
   nothing auto-demotes or rewrites the store.
2. **Honest under sparsity.** Every core abstains loudly on thin data
   (conservative bounds, `abstained` flags, `[0, 1]` intervals) rather than
   emitting a confident-looking number. No guarantee language where
   exchangeability does not hold.

## Status

- **Landed + verified:** the three decision cores (`compute_token_roi`,
  `detect_regime_shift`, `calibration_honesty_report`) with inline unit tests
  plus the property/contract tests in `tests/feedback_gated_properties.rs`
  (`13.4`): conservatism, determinism, ranking, trailing-window-only flips,
  loud abstention.
- **Follow-on (CLI / golden-gated):** the `ee roi pack|memory` + `ee budget
  recommend` + calibration-honesty CLI surfaces, the aggregation queries over
  pack ledgers ↔ outcomes, derived-asset persistence with freshness, the
  `scripts/e2e_feedback_gated.sh` e2e (`13.5`, needs those CLIs), and
  capabilities/agent-docs/help registration (`13.6`).

## See also

- [`dueling-wizards-why-packdna-signals.md`](dueling-wizards-why-packdna-signals.md) — the pack-DNA signals these outcomes are bucketed against.
- [`dueling-wizards-store-integrity.md`](dueling-wizards-store-integrity.md) — write-immune quarantine, the other propose-don't-mutate feedback path.
