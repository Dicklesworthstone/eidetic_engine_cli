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
