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

Operator loop:

1. Collect the smallest structured artifact set that already exists: RCH proof,
   replay ledger, pack diff, perf report, degraded fixture, Beads/BV history, or
   support-bundle summary.
2. Build or inspect the causality capsule. The future CLI surface should return
   `ee.response.v2` with a `data` payload shaped by
   `ee.regression_causality.v1`.
3. Follow only read-only `nextCommands` until evidence proves a source, fixture,
   tracker, or environment owner. Heavy Rust checks stay behind RCH.
4. Record whether the diagnosis helped or misled. If the capsule abstains, file
   a follow-up bead for the missing artifact class instead of promoting a weak
   hypothesis.

## Redacted Examples

### RCH Source-State Mismatch

Inputs:

- `verification_evidence`: `ee.rch.verify.v1` summary with
  `status=rch_environment_failure`, `remote_source_materialized=false`, and
  `source_materialization=remote_checkout_unverified`.
- `git_metadata`: current `git_head`, `git_tree`, and dirty-status hash.
- `beads_history`: blocker bead and retry window by id only.

Expected lead:

- top hypothesis `source_not_materialized`;
- `authoritative: false`;
- next command points at the RCH verifier or blocker ledger;
- no source bead is closed until a proof shows the tested source actually
  matched the current checkout.

### Schema Drift

Inputs:

- `degraded_fixture`: fixture id and expected emission substrings;
- `git_metadata`: schema file hash;
- `e2e_event_log`: failing contract row with test id and artifact hash.

Expected lead:

- top hypothesis `schema_contract_drift`;
- owner hints point at the schema or fixture surface;
- next commands stay in static contract territory, such as `jq empty` and the
  relevant schema/fixture test through RCH if Rust execution is required.

### Perf Regression

Inputs:

- `perf_report`: `ee.perf.v1` budget or compare artifact;
- `swarm_replay`: replay scorecard id and latency percentiles;
- `support_bundle`: redacted bundle manifest id.

Expected lead:

- top hypothesis `perf_budget_regression`;
- counter-evidence names missing workload or host-profile artifacts if present;
- next commands prefer `ee perf explain-latency`, `ee perf compare`, or replay
  inspection before broader benchmark work.

### Pack-Quality Omission

Inputs:

- `pack_replay`: historical pack ledger id and hash;
- `pack_diff`: omitted/added item summary and freshness posture;
- `degraded_fixture`: any stale-index or source-mode degradation that affected
  retrieval.

Expected lead:

- top hypothesis `pack_selection_change`, `stale_derived_asset`, or
  `fixture_gap`, depending on which input is authoritative;
- next commands point at `ee pack replay`, `ee pack diff`, or the missing
  fixture owner;
- raw pack text, raw memory bodies, and private paths remain absent.

## Related Operator Surfaces

- [RCH verification](../rch_verification.md) supplies selector admission,
  source-materialization, known-blocker, and local-fallback evidence.
- [Pack replay](../pack-replay.md) supplies pack replay and pack diff ledgers.
- [Perf forensics](../perf-forensics-cookbook.md) supplies deterministic
  latency, cache, and budget evidence.
- [E2E event contract radar](../e2e-event-contract-radar.md) supplies
  first-failure and contract-drift event rows.
- [Swarm work packets](swarm-work-packet.md) and Beads/BV history supply tracker
  and ownership evidence for crowded checkout failures.
- Support bundles carry `regression_causality_summary.json` as a derived,
  redaction-safe support artifact. It keeps normalized evidence rows, ranked
  non-authoritative hypotheses, summary provenance, and redaction posture while
  omitting raw logs, raw mail bodies, raw memory bodies, host-private paths, and
  copied input artifacts.

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
  paths, secrets, raw mail bodies, or raw memory bodies;
- support-bundle integration that proves
  `ee.support_bundle.regression_causality_summary.v1` is present, parseable,
  hash/provenance based, and non-authoritative.

Later implementation beads should add the agent-facing CLI/report producer,
broader golden fixtures, no-mock e2e logs, and RCH-only focused Cargo proof.
