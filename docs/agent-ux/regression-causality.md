# Regression Causality Contracts

`ee.regression_causality.v1` is a compact, redaction-safe diagnostic capsule
for failed or suspicious verification gates. It answers one question: given the
evidence already produced by `ee`, RCH, Beads, BV, replay, pack, perf, and
support-bundle surfaces, what are the most plausible reasons this gate failed
now?

The capsule is not a verdict engine. It preserves direct evidence separately
from ranked hypotheses. An evidence source with `authoritative: true` is a
summary of an input artifact. A hypothesis always has `authoritative: false`;
it is a deterministic lead for the next inspection step, not proof that source
code is wrong.

## Accepted Inputs

Implementations may normalize these artifact classes:

- `verification_evidence`: `ee.verification_evidence.v1` records from RCH,
  verify scripts, CI job summaries, or static checks.
- `rch_selector_admission`: selector or build-admission records that explain
  remote refusal, topology blockers, source-package refusal, or local fallback
  refusal.
- `swarm_replay`: `ee.swarm_replay_result.v1` result ledgers and scorecards.
- `e2e_event_log`: `ee.test_event.v1` rows and contract-radar summaries.
- `pack_replay` and `pack_diff`: pack replay, omission, and quality reports.
- `perf_report`: `ee.perf.v1`, live perf, budget, and explain-latency reports.
- `beads_history` and `bv_history`: tracker, dependency, owner, and graph
  history summarized by stable ids and hashes.
- `degraded_fixture`: failure-mode catalog fixtures and degraded-code docs.
- `git_metadata`: commit, tree, dirty-status, and source-package hashes.
- `support_bundle`: existing redacted bundle summaries.

The capsule links to inputs by `id`, `schema`, and `artifactHash`; it does not
copy raw logs, full stdout or stderr, raw mail bodies, memory bodies, private
checkout paths, or environment dumps.

## Required Shape

Top-level fields are:

- `schema`: the literal `ee.regression_causality.v1`.
- `subject`: the failing surface, artifact kind/id, command hash, observation
  time if already present, and workspace hash.
- `sourceState`: source materialization posture, local dirty posture, remote
  source-materialization status, source hash, attribution, and degraded codes.
- `evidenceSources`: compact summaries of every accepted input considered.
- `hypotheses`: ranked deterministic causes with confidence, severity,
  supporting evidence refs, counter-evidence, owner hints, next commands, and
  `authoritative: false`.
- `redaction`: safety posture. Raw logs, raw mail bodies, raw memory bodies,
  and private paths are const-false. `secretScanApplied` is const-true.
- `degraded`: capsule-generation gaps that affected the report.
- `nextCommands`: read-only follow-up commands. Heavy Rust proof commands must
  still run through RCH.

The initial hypothesis vocabulary is:

- `source_not_materialized`
- `schema_contract_drift`
- `stale_derived_asset`
- `known_environment_blocker`
- `output_budget_regression`
- `fixture_gap`
- `pack_selection_change`
- `perf_budget_regression`
- `tracker_state_mismatch`
- `unknown_insufficient_evidence`

The source-materialization vocabulary is:

- `committed_tree`
- `dirty_source_materialized`
- `remote_checkout_unverified`
- `source_state_refused`
- `not_applicable`
- `unknown`

## Agent Workflow

Use a causality capsule after a gate fails and at least one structured artifact
already exists. Start with the top hypothesis, inspect its `evidenceRefs`, read
its `counterEvidence`, then run only the relevant `nextCommands`. If a command
would invoke Rust verification, keep it RCH-only and preserve exact blocker
strings when Cargo never starts.

When the top hypothesis is `source_not_materialized`, do not treat the failed
gate as a source verdict. First repair or rerun the remote-source proof. When
the top hypothesis is `unknown_insufficient_evidence`, file a follow-up bead
for the missing artifact instead of guessing from raw logs.

## Test Plan

Contract coverage for this foundation must pin:

- schema identity, top-level required fields, and closed-object posture;
- nested required fields for subject, source state, evidence source,
  hypothesis, counter-evidence, owner hint, command, redaction, and degraded
  entry objects;
- enum vocabularies for subject surface, evidence kind/status, redaction
  status, hypothesis code, source materialization, severity, owner hint kind,
  and counter-evidence effect;
- field presets for minimal, summary, standard, and full output;
- redaction consts and an example that does not contain raw logs, host-private
  paths, secrets, raw mail bodies, or raw memory bodies.

Later implementation beads should add producer tests, golden fixtures, no-mock
e2e logs, support-bundle integration, and RCH-only focused Cargo proof.
