# Agent Workload Replay

`ee lab swarm replay --trace <workload.json> --dry-run --json` turns a
redaction-safe `ee.swarm_workload.v1` fixture into a deterministic
admission-only `ee.swarm_replay_result.v1` ledger. Use
`ee lab promote-workload --trace <trace.jsonl> --agents 64 --json` first when
the source is a recorded `ee.agent_workload_trace.v1` JSONL file.

The promotion harness is read-only. It does not call Agent Mail, Beads, RCH,
the workspace database, search indexes, or external services. It consumes only
redacted JSONL trace rows and emits deterministic workload metadata: command
counts, schemas observed, degraded-code posture, byte/token deltas, inferred
cache hit/miss posture, duplicate-work coalescing posture, and stable BLAKE3
hashes for fixture promotion.

The replay harness consumes the promoted `ee.swarm_workload.v1` JSON file and
emits the result ledger. The deterministic hash input strips volatile
`recordedAt` timestamps and sorts source rows by stable redacted shape before
promotion. Replaying the same fixture produces the same workload and replay
hashes. The default recorded fan-out is 64 agents; larger requests are capped
and reported through resource-limit fields and `warnings[]` instead of
silently oversubscribing the host.

Raw task strings, query text, memory bodies, provenance text, mail bodies,
environment dumps, file listings, or secrets are never required. If a trace row
claims any raw-content posture is present, replay fails with a policy-denied
error instead of building a fixture.

Smallest useful smoke:

```bash
ee --workspace . --json lab generate-workload \
  --fixture-seed smoke_replay_lab_smoke_001 \
  --profile small > /tmp/ee-swarm-workload-smoke.json

ee --workspace . --json lab swarm replay \
  --trace /tmp/ee-swarm-workload-smoke.json \
  --dry-run
```

The smoke command is side-effect-free and should emit
`ee.swarm_replay_result.v1`. Without an attached RCH proof it exits with the
degraded-required code and reports `verification.rchStatus:
"blocked_before_cargo"` plus `swarm_replay_rch_proof_missing`; that is the
expected smoke posture, not scale proof.

Example trace tiers:

```bash
# CI smoke: four agents, six command shapes, admission only.
ee --workspace . --json lab generate-workload \
  --fixture-seed smoke_replay_lab_smoke_001 \
  --profile small > /tmp/ee-swarm-workload-smoke.json

# Standard crowded-checkout trace for RCH-backed replay verification.
ee --workspace . --json lab generate-workload \
  --fixture-seed standard_replay_lab_001 \
  --profile medium > /tmp/ee-swarm-workload-standard.json

# Large-host trace for 256GB+/64-core operators; do not run locally on laptops.
ee --workspace . --json lab generate-workload \
  --fixture-seed large_host_replay_lab_001 \
  --profile large > /tmp/ee-swarm-workload-large-host.json
```

RCH-only standard proof:

```bash
RCH_REQUIRE_REMOTE=1 ./scripts/rch_verify.sh \
  --bead-id bd-ppbue.8 \
  --summary \
  --no-write \
  --known-blocker-override \
  -- cargo test --test e2e_lab_swarm_workload_generator \
  lab_swarm_replay_executes_small_generated_fixture_with_artifact_ledger \
  -- --nocapture
```

RCH-only large-host proof:

```bash
RCH_REQUIRE_REMOTE=1 ./scripts/rch_verify.sh \
  --bead-id bd-ppbue.8 \
  --summary \
  --no-write \
  --known-blocker-override \
  -- cargo test --test e2e_lab_swarm_workload_generator \
  lab_generate_workload_emits_all_profiles_as_redaction_safe_json \
  -- --nocapture
```

The local verification hook is `scripts/e2e_overhaul/swarm_replay_lab_smoke.sh`.
It does not invoke Cargo. It records `ee.test_event.v1` command and assertion
events with sanitized environment posture, elapsed time, exit code,
stdout/stderr artifact references, schema validation status, redaction status,
and first-failure diagnosis.

Promotion flow:

1. Capture or export redacted `ee.agent_workload_trace.v1` JSONL rows.
2. Run `ee lab promote-workload --trace trace.jsonl --agents 64 --json`.
3. Run `ee lab swarm replay --trace promoted-workload.json --dry-run --json`.
4. Attach an RCH proof capsule for non-smoke replay closeout.
5. Commit the sanitized fixture and use `fixturePromotion.perfBudgetKey` as the
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
