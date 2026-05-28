# ADR 0050: Active-learning curate-candidate selection (N11) — defer to research backlog

Status: Deferred (research backlog)
Date: 2026-05-27
Bead: bd-17c65.14.11 (N11)

## Context

`ee curate candidates` today ranks pending procedural-rule candidates by a
deterministic priority blend (confidence, utility, freshness, harmful-feedback
penalty). bd-17c65.14.11 (N11) proposed replacing that with an active-learning
policy that selects the next candidate whose validation outcome maximally
reduces uncertainty about the rule-acceptance distribution, using
information-gain over the N7 Beta-Bernoulli posteriors:

```
info_gain(c) = H(Beta(α, β)) − E_outcome[H(Beta(α', β'))]
```

The premise is that a human curator validating in active-learning order
reaches a stable accept/reject ratio in fewer iterations than FIFO or
priority-sorted order, with the design-of-experiments target being ≥ 30 %
reduction in validations on the eval fixture.

Prerequisites N2 (conformal calibration), N4 (typed determinism), N7 (Bayesian
posteriors), N11.3 (degraded-code docs), and bd-17c65.7 (learn/curate honest
implementation) are all closed or shippable. The remaining blocker is whether
the labeled corpus we have at scale today justifies the engineering cost and
the human-curator friction of an active-learning queue.

## Decision

**Defer N11 to the research backlog.** The current heuristic priority is
"good enough" against the eval-fixture acceptance ratio, and the labeling
corpus we have for offline evaluation is too small to meaningfully measure
the predicted ≥ 30 % reduction. The decision is not "never"; it is "not
until the curate-acceptance ledger crosses a corpus-size threshold that
makes the offline eval statistically credible."

Specifically:

1. Do not implement `ee curate candidates --policy active-learning` in the
   current milestone window.
2. Do not change the existing deterministic priority ordering.
3. Do not add an `active_learning` config key; introducing the surface
   without the implementation would create another stub-vs-real honesty
   gap of the kind AGENTS.md's honesty-only/implements-surface taxonomy
   was written to prevent.
4. Re-evaluate the deferral when the workspace's accept/reject ledger
   reaches the corpus-size threshold below, OR when offline experiments
   on synthetic labeled data show a stable ≥ 30 % reduction in validations
   to reach the eval-fixture accept ratio.

### Re-open Criteria

- Accept/reject ledger ≥ 1,000 labeled candidates across ≥ 3 procedural-rule
  families (today: order of magnitude below that).
- Offline simulation on a synthetic Beta-prior corpus shows the predicted
  ≥ 30 % reduction holds across at least three independent seeds.
- A separate curator-UX bead surfaces operator pain with the existing FIFO
  / priority queue.

## Consequences

What becomes easier:

- N11 stops surfacing as "ready" work in `br ready` because its acceptance
  row ("≥ 30 % reduction in validations") cannot be honestly measured against
  the current corpus.
- The curate surface stays deterministic and explainable; same input → same
  candidate order → same pack hash. No hidden classifier state.
- Operators do not learn an active-learning queue shape that may be reversed
  if the offline eval contradicts the design hypothesis.
- The work backlog focuses on shippable next-milestone items (NUMA pinning,
  Linux mmap adapter, shard fanout, watchdog integration) instead of
  research-grade signal-processing work.

What becomes harder:

- If a curator does want lower-validation-count convergence sooner, the
  product gap remains until the re-evaluation triggers fire.
- The N11 spike — when it runs — has to re-derive its own offline-eval
  fixture rather than inheriting one that grew naturally from production.

## Rejected alternatives

1. **Ship a minimal active-learning queue today.** The ≥ 30 % reduction
   acceptance bar cannot be measured against the current ledger size, so
   "shipped" would mean an unverified feature behind a flag — exactly the
   pattern AGENTS.md's implements-surface taxonomy is meant to prevent.
2. **Ship the information-gain math only and gate the surface behind a
   feature flag.** Same honesty problem — `--policy active-learning` would
   exist but `info_gain` numbers would not be benchmarked against any
   convergence target. Operators reading `ee curate candidates --explain`
   would see a metric whose contract is "we hope this is useful."
3. **Replace the deterministic priority blend with active-learning
   unconditionally.** Changes the determinism contract (`same DB +
   indexes + config + query → byte-identical JSON`) without a verified
   payoff. Rejected for the same reason as (1) plus the determinism
   regression risk.
4. **Spike active-learning on synthetic data only and decide later.** A
   spike without production labels predicts neither the acceptance ratio
   nor the curator-UX impact of an active-learning queue order. Useful as
   a research artifact, not as a shipping decision.

### Candidate approaches considered (for the future spike)

- **Margin sampling:** pick the candidate whose Beta posterior mean is
  closest to the accept/reject decision boundary. Cheapest, but assumes
  the boundary is the only uncertainty axis.
- **BALD (Bayesian active learning by disagreement):** select candidates
  that maximize mutual information between the label and the policy
  parameters. Theoretically optimal under the Beta-Bernoulli model;
  needs Monte Carlo sampling because the closed-form gradient is small.
- **Query-by-committee:** train K shadow classifiers on bootstrapped
  subsamples of the accept/reject ledger; pick candidates with maximal
  vote disagreement. Sidesteps the model-form question; needs the
  bootstrap corpus N11 doesn't yet have at scale.

Each is referenced here for the future spike, not selected by this ADR.

## Verification

How a future reviewer can confirm this decision remains valid:

- `br show bd-17c65.14.11` reports `status: deferred` (or closed with
  `Shipped ADR 0050`) and references this ADR by id.
- `docs/adr/README.md` lists ADR 0050 under "Deferred / research backlog".
- `grep -RFq "policy active-learning" src/cli/mod.rs` returns no match
  (no `--policy active-learning` flag has been added to the curate
  surface).
- `grep -RFq "info_gain" src/core/curate.rs src/core/learn.rs` returns
  no match (no info-gain emission has been added to the curate
  candidate output).
- The curate priority sort in `src/core/curate.rs` continues to use the
  deterministic confidence × utility × freshness blend; if a future
  contributor changes it, the burden of proof for the change names this
  ADR explicitly.

If any of those checks fail without a superseding ADR, the deferral has
silently drifted and the next reviewer should either restore the
deferred state or open a new ADR superseding this one with the offline
evaluation evidence the re-evaluation triggers above require.
