# Agent Workload Replay

`ee lab replay --trace <trace.jsonl> --agents 64 --json` turns redacted
`ee.agent_workload_trace.v1` rows into a deterministic synthetic
64-agent `ee.agent_workload_replay.v1` report.

The replay harness is read-only. It does not call Agent Mail, Beads, RCH, the
workspace database, search indexes, or external services. It consumes only the
redacted JSONL rows and emits aggregate command counts, schemas observed,
degraded-code deltas, byte/token deltas, p50/p95/p99 latency, inferred
cache hit/miss posture, duplicate-work coalescing posture, and stable BLAKE3
hashes for fixture promotion.

The deterministic hash input strips volatile `recordedAt` timestamps and sorts
rows by stable redacted shape. Replaying the same fixture in a different row
order produces the same `traceHash`, `playback.workloadHash`, `replayHash`,
and JSON report. The default synthetic fan-out is 64 agents; larger requests
are capped and reported through `playback.resourceLimited` plus `warnings[]`
instead of silently oversubscribing the host.

Raw task strings, query text, memory bodies, provenance text, mail bodies,
environment dumps, file listings, or secrets are never required. If a trace row
claims any raw-content posture is present, replay fails with a policy-denied
error instead of building a fixture.

Promotion flow:

1. Capture or export redacted `ee.agent_workload_trace.v1` JSONL rows.
2. Run `ee lab replay --trace trace.jsonl --agents 64 --verify-determinism --json`.
3. Commit the sanitized fixture and use `fixturePromotion.perfBudgetKey` as the
   stable key for downstream budget rows.

## Swarm SLO Scorecards

`ee.swarm_slo.scorecard.v1` is the workload-level SLO contract for multi-agent
replays. It deliberately reuses `ee.agent_workload_trace.v1` as its trace row
input instead of defining another workload trace schema. The trace schema is
already the redaction-safe command-shape contract: it records verb chains, flag
names, timing, response sizes, hashed memory refs, and degraded codes without
raw task strings, query text, memory bodies, mail bodies, command output,
secrets, environment dumps, or private path listings.

The scorecard adds the aggregate layer that a 64-agent swarm needs: agent count,
concurrency shape, command mix, source postures, synthetic-vs-recorded
provenance, expected degradation posture, named SLO budget profile, p50/p95/p99
latency, error/degraded counts, stage attribution, deterministic replay hashes,
budget verdicts, and actionable regression reasons. The canonical budget
profiles are `ci_smoke`, `developer_crowded_checkout`, `swarm_heavy_64_agent`,
and `stress_256gb_host`.

Every scorecard includes `budgetVerdicts[]` rows for the measured budgets that
decided the verdict. Failing and blocked scorecards also include
`regressionReasons[]` with stable codes such as `context_p99_over_budget`,
`coordination_source_unavailable`, and `rch_topology_blocked`, plus a repair
string that tells an agent which prerequisite to fix before rerunning the
release gate. Fixtures under
`tests/fixtures/golden/swarm_slo_scorecard/` pin the healthy small checkout,
crowded checkout, Agent Mail unavailable, BV timeout/no-output, and RCH topology
blocked scenarios without requiring live external services.
