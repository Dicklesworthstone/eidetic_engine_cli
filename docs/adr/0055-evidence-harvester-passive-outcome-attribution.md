# ADR 0055: Evidence Harvester — passive, audited outcome attribution

Status: proposed
Date: 2026-06-07
Bead: bd-1n0np.2.1

## Context

Almost every adaptive behavior `ee` already ships is driven by **outcome
feedback**: Bayesian confidence posteriors (alpha/beta with `harmful_weight`),
decay half-lives, level promotion, trust-class transitions, per-agent bias, and
ranking utility. Yet outcomes are recorded **only manually** — `src/core/outcome.rs`
exposes `record_outcome` / `record_outcome_seeded` and nothing else. In real
agent workflows almost no one runs `ee outcome <id> --signal helpful` after the
work is done, so the feedback that the whole value chain assumes will exist is
chronically starved. The flywheel the product is built around barely turns.

This is the single highest-leverage gap identified by the 2026-06-07 multi-model
"dueling idea wizards" review: it was the top-scored idea (918) and the one both
external models independently named as the systemic constraint on `ee`'s
"learning" features. A whole class of otherwise-inert downstream ideas (token
ROI, temporal regime-shift, honest calibration) cannot reach statistical
significance until a passive outcome stream exists; this ADR is therefore the
keystone that unblocks them (tracked as the feedback-gated layer, bd-1n0np.13).

The substrate to do this **already exists**: the pack selection ledger
(ADR 0025) records the exact contributing memory IDs; the flight recorder
(`src/core/recorder.rs`) links pack → run → task; `completion_audit.rs` already
parses `.beads/issues.jsonl` for bead close/reopen; `VerificationEvidenceRecord`
captures verification pass/fail; and the bounded source-runner used by
`swarm_brief.rs` can read git state. No new collection is required — only a join
over observations `ee` already makes.

The controlling-idea constraint is strict: `ee` is the memory layer, not the
agent loop. It may **observe** git/beads/CI *after* the agent acts; it must never
schedule, claim, or drive work.

## Decision

Introduce an **impression-and-outcome ledger** plus a deterministic joiner that
produces **derived, low-weight, fully-audited** outcome signals, exposed through
`ee outcome harvest` and `ee outcome calibration`.

1. **Impression record** (written at pack assembly): `pack_id`, `memory_id`,
   `query_hash`, `lens_hash`, `rank`, `section`, `token_estimate`, `selected`,
   and the derived-asset generations in play. Reuses pack-ledger (ADR 0025) and
   recorder join keys.
2. **Outcome-evidence record** (derived from existing observations only):
   verification passed/failed, bead closed-clean/reopened, commit landed/reverted,
   plus explicit human/agent `helpful|harmful`.
3. **Deterministic joiner**: links impressions to outcome evidence **only** within
   **explicit RFC3339 windows** (never `Date::now`) and **only** when
   task/workspace/proof lineage match.
4. **Source-reliability weighting**, strongest → weakest: explicit human >
   explicit agent (agent-scoped) > verifier-success-tied-to-pack (weak positive)
   > reverted-patch-tied-to-pack (weak negative) > task-close-without-proof
   (very weak positive) > reopened-task (weak negative).
5. **Hard invariants** (the safety core, bd-1n0np.2.4): derived feedback carries
   a low base weight, requires **≥2 corroborating signals**, **never overrides**
   explicit human/agent feedback, and **never directly promotes or tombstones** a
   memory. It updates a separate derived-feedback table and/or raises curation
   candidates only.
6. **Self-reinforcement guard**: derived contribution per memory per window is
   capped, reusing the existing `[feedback] harmful_per_source_per_hour` /
   `EE_HARMFUL_PER_SOURCE_PER_HOUR` quarantine pattern, so derived outcomes can
   never inflate confidence that drives more selection in a runaway loop.
7. **Calibration** (`ee outcome calibration`): a bounded steward report comparing
   stored confidence against later outcomes (per-situation-class reliability
   buckets + Brier score) and proposing recalibration of `harmful_weight` /
   half-lives. Report-only; no silent mutation.

`ee outcome harvest --dry-run` lists every proposed derived outcome with its
evidence chain and weight; `--apply` writes through the single write-owner
(ADR 0013) and `record_outcome_seeded` for byte-determinism, with audit rows.

## Consequences

- **Easier**: every adaptive feature that depends on outcome density (decay,
  promotion, trust transitions, per-agent bias, ranking utility, and the entire
  feedback-gated layer) starts working as designed instead of on starvation
  rations — without asking users to label anything.
- **Harder / guarded**: attribution is noisy (a packed memory may not be *why* a
  bead closed). This is contained by low derived weight, the ≥2-corroboration
  rule, dry-run-first, full audit (a bad harvest is reversible), and the
  per-window cap.
- **Intentionally impossible**: derived signals cannot override explicit human
  intent, cannot auto-promote/auto-tombstone memory, and cannot run during a
  determinism-sensitive path with wall-clock windows.
- Determinism is preserved: identical DB + explicit windows → byte-identical
  proposals and calibration report.

## Rejected Alternatives

- **Infer success purely from commits** ("commit landed ⇒ helpful"): too crude
  and confound-prone; rejected in favor of multi-signal corroboration with
  reliability weights.
- **Let derived signals feed confidence directly** (no separation): violates
  no-silent-mutation and risks self-reinforcing loops; rejected for a separate
  derived-feedback table + curation candidates.
- **A learned model over outcomes** (e.g. embedding projection): high cost,
  determinism friction, and data-starved at v1; deferred to the feedback-gated
  layer and only after this ledger produces dense labels.
- **Keep outcomes manual and exhort agents to call `ee outcome`**: the status
  quo; empirically does not happen, which is the entire motivation.

## Verification

- Unit tests prove the safety invariants directly: derived-never-overrides-explicit,
  ≥2-corroboration gating, the per-window cap, weight ordering, and deterministic
  proposals over explicit windows (bd-1n0np.2.8).
- A real-binary e2e (`scripts/e2e_evidence_harvester.sh`, bd-1n0np.2.9, on the
  shared harness) drives impression → simulated bead-close/verification/revert →
  `harvest --dry-run` proposals with weights → `--apply` with audit →
  explicit-signal-not-overridden → `calibration` report.
- Determinism gate (bd-1n0np.15.2) asserts byte-identical `ee.outcome.harvest.v1`
  and `ee.outcome.calibration.v1` across runs.
- New `EE_*` window/weight knobs are registered in `src/config/env_registry.rs`
  and the new-surface contract guard (bd-1n0np.15.3) asserts capabilities /
  agent-docs / help registration.
