# ADR 0032: Bayesian (alpha, beta) Posteriors on Memories

Status: accepted
Date: 2026-05-13

## Context

Each memory carries a `confidence` and a `utility` value updated on
every `ee outcome` event. Today those updates are *ad-hoc linear
deltas*: a helpful outcome adds a small positive amount to both
fields; a harmful outcome subtracts a larger amount per the
README-documented `harmful_weight = 2.5` asymmetry. The deltas drift
the scalar values, but the system has no notion of *how confident we
are in the confidence itself*.

Two concrete failure modes follow from that gap:

1. **Trust-class flicker.** ADR 0009 (Trust Class Taxonomy) ties
   trust-class transitions to point-estimate threshold crossings on
   the `confidence` field. A memory hovering near a boundary bounces
   between classes as its scalar confidence wobbles up and down with
   each outcome event. The trust class — which gates packing
   priority, redaction policy, prompt-injection defense — is
   therefore not a stable property of a memory.

2. **Sample-size blindness.** A memory with two helpful events and
   zero harmful events has the same `confidence = 0.85` as a memory
   with two thousand helpful events and zero harmful events. The
   downstream consumer cannot distinguish "novel observation" from
   "exhaustively validated" without consulting a separate count
   field. No credible interval is available; no calibration is
   possible.

`bd-17c65.14.7` (N7) was filed against these gaps as an
ambition-tier upgrade. Its umbrella structure mirrors `bd-17c65.14.15`
(N15) and `bd-17c65.14.5` (N5): an ADR sub-bead gates the
implementation sub-bead. This ADR (`bd-17c65.14.7.1`) is that gate.

The ADR must commit to a posterior model, a prior choice, an update
rule, a backfill plan for existing memories, and an explicit
amendment to ADR 0009 trust-class transitions. The decision affects
every consumer of memory confidence (retrieval scoring, pack
priority, trust pipeline, redaction policy, prompt-injection guard,
audit log shape, and the N11 and N12 ambition beads that depend on
this posterior).

## Decision

We track per-memory **Beta-Bernoulli posteriors** with a Jeffreys
prior, update them on every outcome event, and use the 90 percent
credible interval — not the point estimate — to drive trust-class
transitions.

### Posterior model

Each memory carries two new columns:

- `bayes_alpha REAL NOT NULL DEFAULT 0.5`
- `bayes_beta REAL NOT NULL DEFAULT 0.5`

These parameterize a `Beta(alpha, beta)` posterior over the latent
helpful-rate of the memory. The Beta distribution is the conjugate
prior for Bernoulli observations: helpful = 1, harmful = 0.

Derived fields surfaced inside `ee.bayes.posterior.v1` (the new
sub-envelope embedded in `ee why <memory-id>` output and other
consumers):

- `mean = alpha / (alpha + beta)` — the point estimate; this is what
  the existing `confidence` field continues to expose for backward
  compatibility within the same envelope version.
- `credible_interval_90 = (Beta.ppf(0.05; alpha, beta), Beta.ppf(0.95;
  alpha, beta))` — the 90 percent equal-tailed credible interval.
- `credible_interval_50 = (Beta.ppf(0.25; alpha, beta), Beta.ppf(0.75;
  alpha, beta))` — the 50 percent interval; rendered in compact
  formats that drop the 90 percent interval.

### Prior choice: Jeffreys (0.5, 0.5)

The Jeffreys prior is the reference prior for Bernoulli observations:
invariant under reparameterization, minimally informative without
being improper. A brand-new memory with no observations has
`Beta(0.5, 0.5)` — mean = 0.5, wide credible interval `(~0.015,
~0.985)` at the 90 percent level — correctly reflecting that we have
no information about its helpful-rate.

Configuration keys `[bayes] prior_alpha` and `[bayes] prior_beta`
allow operator override per workspace, but the default ships as
Jeffreys.

### Update rule

On a helpful outcome event: `alpha += 1.0`.

On a harmful outcome event: `beta += harmful_weight`. The
`harmful_weight` defaults to 2.5 per the existing
`[curation] harmful_weight` configuration; the asymmetry is preserved
from the existing scalar-delta system.

Updates are atomic per memory under the L1 write-owner discipline
(ADR 0013). Each update emits a new audit-log action
`memory.bayes_posterior_updated` with the prior `(alpha, beta)`, the
event signal and weight, and the posterior `(alpha, beta)`.

### Trust-class transitions (amends ADR 0009)

ADR 0009 enumerated five trust classes with initial confidences but
left the transition rules to implementation. This ADR pins the
transition rules to *credible-interval boundary crossings*, not
point-estimate thresholds. Transitions are audited via a new audit
action `trust_class.transition`.

| From              | To                | Promote condition                                            | Demote condition |
| ----------------- | ----------------- | ------------------------------------------------------------ | ---------------- |
| `legacy_import`   | `cass_evidence`   | `ci90.lo > 0.50` AND at least 1 validation event             | `ci90.hi < 0.30` |
| `cass_evidence`   | `agent_assertion` | `ci90.lo > 0.60`                                             | `ci90.hi < 0.35` |
| `agent_assertion` | `agent_validated` | `ci90.lo > 0.70` AND `alpha + beta >= 6` (sample-size gate)  | `ci90.hi < 0.40` |
| `agent_validated` | `human_explicit`  | NOT AUTOMATIC — requires explicit `ee rule promote --to human_explicit` | `ci90.hi < 0.45` |

Two structural choices in this table need explicit rationale:

- **Promotion thresholds use `ci90.lo` (the conservative lower
  bound); demotion thresholds use `ci90.hi` (the conservative upper
  bound).** This is *hysteresis*: a memory whose credible interval
  just barely qualifies it for promotion will not bounce back on the
  next non-helpful event, because demotion of the next class down
  requires the upper bound to fall further. The gap between
  promotion thresholds and demotion thresholds (10–15 percentage
  points) is the hysteresis band.
- **The `agent_assertion -> agent_validated` transition has an
  explicit sample-size gate: `alpha + beta >= 6`.** Without it, a
  small `(alpha = 4, beta = 0)` would have `ci90.lo > 0.7` despite
  the thin evidence, and the class would over-fire on first
  observations.

`human_explicit` is the highest class and is never reached
automatically; an operator must run `ee rule promote --to
human_explicit` to commit a memory into it. Demotion from
`human_explicit` is still automatic on `ci90.hi < 0.45` so a memory
explicitly marked human-trusted but persistently harmful does not
remain authoritative.

### Backfill for existing memories

The schema migration adds `bayes_alpha` and `bayes_beta` columns with
default Jeffreys (0.5, 0.5). Existing rows therefore have a clean
prior with no observations, regardless of their accumulated
`confidence` or `utility` values today.

Two opt-in backfill modes (run via `ee migrate run`) let operators
choose a richer migration:

- `--bayes-backfill-from-utility`: derive `(alpha, beta)` from the
  existing `utility` and `confidence` fields via inverse fitting
  with weight = 2. For example, a memory with `confidence = 0.8`
  backfills to `(alpha = 1.6, beta = 0.4)`. This preserves the
  per-row trust-class observation but assumes the existing scalar
  carries low-evidence information.
- `--bayes-backfill-from-feedback-events`: replay every
  `feedback_events` row through the Beta-Bernoulli update rule from
  scratch, starting from the Jeffreys prior. This is the strongest
  backfill, the most expensive (proportional to total event count),
  and the most evidence-faithful. It is only meaningful when the
  `feedback_events` table has data; for workspaces that pruned old
  events, it degrades to the default.

The default (no flag) is recommended for fresh installs and small
workspaces where treating existing memories as "no signal yet" is
acceptable. The backfill modes exist for workspaces with significant
history that would otherwise reset trust-class state in a way the
operator does not want.

### Schema migration reversibility

The migration uses the L0 framework (`bd-17c65.12.5`) and is
reversible. `down()` removes both columns. A round-trip
(`up -> down -> up`) leaves identical state as a fresh-up. Tests
under `bd-17c65.14.7.2` (N7.1) gate this property in CI.

## Consequences

What becomes easier:

- **Trust class is stable.** Hysteresis bands eliminate flicker. A
  memory in `agent_validated` does not demote on the first
  non-helpful event; it demotes only when the upper credible bound
  has clearly fallen.
- **Sample size is observable.** `alpha + beta` is the effective
  count; consumers can ask "how much evidence supports this
  memory?" without consulting a separate field.
- **Conformal calibration (N2) and active-learning (N11) compose
  naturally.** Both ambition beads consume the Beta variance
  directly: N2 uses calibration sets derived from posteriors; N11
  ranks candidates by information gain, which is closed-form on
  Beta posteriors.
- **The trust pipeline becomes auditable.** Every transition is
  emitted as a `trust_class.transition` audit-log entry with the
  triggering credible-interval values. An operator can trace why a
  memory's class changed without re-running outcome events.

What becomes harder:

- **Storage cost increases by 16 bytes per memory** (two `REAL`
  columns). For 14k memories per the README perf baseline, that is
  ~220 KB total — negligible.
- **`ee why` output grows.** Per `bd-17c65.14.7.2` Round 5 budget
  spec, the per-memory `bayesPosterior` block adds ~120 bytes JSON;
  the response-size budget allows up to 10 percent overhead over
  pre-N7.1 baseline.
- **ADR 0009 is amended.** Downstream readers must consult both
  ADR 0009 (for trust-class definitions and initial confidences) and
  this ADR (for transition rules). A future merge ADR can fold them.
- **The existing scalar `confidence` field stays present for
  envelope-version compatibility but is now a derived view over the
  Beta posterior** (`confidence = alpha / (alpha + beta)`). Removing
  it is a future envelope-version bump.

What becomes intentionally impossible:

- **Setting `confidence` directly without an outcome event.** The
  trust pipeline now treats `confidence` as a *derived value*, not a
  primary field. An operator who wants to express "I trust this
  memory at 0.8" must either invoke `ee outcome --signal helpful`
  the appropriate number of times or run `ee rule promote --to
  <class>` explicitly. The previous "scalar trust" workflow is gone
  by design.

## Rejected Alternatives

### Keep scalar deltas; track a sample count separately

Add a `feedback_count INTEGER` column alongside `confidence`; do
nothing else. Use the count to suppress promotion until it exceeds a
threshold.

Rejected because:

- The point estimate still flickers without a credible interval;
  sample count alone does not give us hysteresis.
- Backfill is still ambiguous (which point of the existing
  confidence corresponds to which count?).
- N2 and N11 need a *distribution*, not a count; we would end up
  building Beta posteriors on top of this anyway.

### Gaussian posterior

Track `(mean, variance)` of a Gaussian over the latent helpful-rate.

Rejected because:

- The Gaussian has unbounded support; the latent rate is bounded in
  `[0, 1]`. A Gaussian assigns nonzero probability to negative or
  >1 rates.
- Updating a Gaussian on Bernoulli observations is awkward: a
  Beta-Bernoulli model is the standard conjugate solution; reaching
  for a Gaussian would be inventing problems.
- The Gaussian credible interval is symmetric; the Beta credible
  interval correctly skews near the bounds, which is exactly the
  behavior we want at high or low confidence.

### Dirichlet over a richer outcome space

Track a Dirichlet posterior over (helpful, harmful, neutral,
contradicted) outcome categories.

Rejected because:

- We only have helpful/harmful in the current outcome taxonomy.
  Extending to neutral and contradicted is a separate question that
  belongs to a curation-system ADR, not this one.
- Beta is the marginal of Dirichlet for the binary case; we can
  upgrade to Dirichlet later without renaming the fields by adding
  more columns alongside `bayes_alpha` and `bayes_beta`.

### Point-estimate thresholds with elapsed-time hysteresis

Keep ADR 0009 unchanged (point-estimate transitions) but add a
minimum elapsed time between transitions (e.g. "no demotion within 1
hour of the previous transition").

Rejected because:

- Elapsed-time hysteresis is fragile under burst events: a single
  harmful event followed by 1 hour of silence still flickers; many
  small helpful events do not promote.
- Time-based hysteresis introduces *non-determinism* into the trust
  pipeline (wall-clock dependencies). Deterministic test gates
  (J7 harness) would have to either fake the clock or accept
  unstable goldens. Credible-interval hysteresis is wall-clock-free.

## Verification

This ADR ships with three static tests under the closure contract:

1. `adr_0032_exists_and_is_indexed` asserts that
   `docs/adr/0032-bayesian-memory-posteriors.md` exists and appears
   in `docs/adr/README.md`.
2. `adr_0032_has_required_sections` asserts that the file contains
   `## Context`, `## Decision`, `## Consequences`, `## Rejected
   Alternatives`, and `## Verification` headers.
3. `adr_0032_documents_required_design_choices` asserts that the
   ADR documents the load-bearing decisions: Jeffreys prior, the
   `harmful_weight = 2.5` asymmetry, the `bayes_alpha`/`bayes_beta`
   column names, the credible-interval-driven transitions, and the
   sample-size gate at `alpha + beta >= 6`.
4. `adr_0032_lists_rejected_alternatives_with_reasoning` asserts the
   rejected alternatives section enumerates at least four concrete
   alternatives with explicit rejection rationale per AGENTS.md ADR
   requirements.

When `bd-17c65.14.7.2` (N7.1) lands the implementation, additional
verification is owned by that bead:

- `tests/bayesian_posterior_update_unit.rs` — known outcome stream
  produces expected `(alpha, beta)` and mean.
- `tests/bayesian_credible_interval_unit.rs` — nominal 90 percent
  credible interval contains the true rate at calibrated frequency.
- `tests/bayesian_harmful_asymmetry_unit.rs` — harmful event adds
  `harmful_weight` to beta, helpful adds 1 to alpha.
- `tests/trust_class_transition_table_unit.rs` — every row of the
  transition table from this ADR is exercised; promotion and
  demotion both verified; the sample-size gate fires correctly.
- `tests/trust_class_transition_audit_unit.rs` — transitions emit
  `audit_log` entries with action `trust_class.transition` and the
  triggering credible-interval values.
- `tests/migration_reversibility_unit.rs` — schema `up -> down -> up`
  leaves identical state.
- `tests/why_renders_credible_interval_unit.rs` — `ee why <id>
  --json` includes the `bayesPosterior` block with the expected
  fields.
- `tests/why_output_size_budget_unit.rs` — response size stays within
  the 10 percent overhead budget defined in `bd-17c65.14.7.2`
  Round 5 spec.

CI gate via `verify.sh` contract stage. Closure-lint
(`scripts/closure-lint.sh --audit --json`) treats `bd-17c65.14.7.1`
as closed only when the ADR-presence tests pass; treats
`bd-17c65.14.7.2` as closed only when the implementation tests above
pass and the implements-surface label matches.

## Related

- `bd-17c65.14.7` — N7 umbrella feature bead.
- `bd-17c65.14.7.2` — N7.1 implementation sub-bead (this ADR gates
  it).
- ADR 0009 — Trust Class Taxonomy (amended by this ADR for
  transitions; class definitions and initial confidences remain
  load-bearing from 0009).
- ADR 0013 — Single Write Owner Actor (governs atomicity of
  posterior updates under concurrent writes).
- `bd-17c65.12.2` — L1 concurrent write contention (N7.1 depends on
  it for the update path).
- `bd-17c65.12.5` — L0 migration framework (N7.1 schema migration
  uses it).
- `bd-17c65.14.11` — N11 active-learning curation (consumes the
  Beta variance for closed-form info-gain).
- `bd-17c65.14.12` — N12 anti-patterns from failed outcomes
  (consumes the posterior mean for severity classification).
- `bd-17c65.14.2` — N2 conformal calibration (consumes outcome-event
  history through the same audit log).
