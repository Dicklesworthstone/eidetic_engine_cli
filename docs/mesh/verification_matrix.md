# SRR6 Mesh Verification Matrix

Status: proposed
Bead: bd-26d7w
ADR: docs/adr/0037-optional-mesh-memory.md

## Purpose

SRR6 mesh work is optional, local-first, and policy-gated. This matrix defines
the shared proof and logging contract for all SRR6 implementation and test
beads so each slice produces comparable evidence instead of one-off scripts.

Every mesh verifier must prove two things:

- Mesh-off behavior is indistinguishable from ordinary local `ee` behavior
  unless mesh is explicitly enabled.
- Mesh-on behavior is deterministic, provenance-preserving, privacy-safe, and
  scoped to the active workspace and peer group.

## Evidence Matrix

| Evidence | Required scope | Normal verify | RCH friendly | Optional long-running |
| --- | --- | --- | --- | --- |
| Unit | Pure parsers, policy decisions, namespace binding, retry math, redaction decisions | Yes, through `cargo test --lib` | Yes | No |
| Integration | CLI JSON contracts, DB/repository rows, import ledgers, cache rows | Yes, focused test targets | Yes | No |
| E2E | Mesh-off, two-node local fixture, three-node replay/partition fixtures | Yes for mesh-off and no-live-service fixtures | Yes when expressed as `cargo test` or shell with an existing binary | Real Tailscale smoke only |
| Golden | Stable JSON, support bundle, handoff, status/doctor, event envelopes | Yes | Yes | No |
| Perf | Search freshness probes, two-tier latency budgets, cache hit paths | No by default | Yes for compare-only benches | Full benchmarks |
| Privacy | Redaction, body/embedding denial, workspace isolation, support bundle leaks | Yes | Yes | No |
| Failure mode | Missing peer binding, stale revision, partition, authorization denied, cache quota | Yes for synthetic fixtures | Yes | Fault-injection soak |
| Model checks | Anti-entropy, stale-read bounds, convergence/idempotence invariants | No by default | Yes for bounded deterministic harnesses | Larger state spaces |

`scripts/verify.sh` should run the normal-verify rows once their backing
features exist. Optional long-running checks must be explicit opt-in and must
not gate local-first non-mesh work.

## Fixture Layout

Use these paths for new SRR6 tests:

```text
tests/fixtures/mesh/
  <scenario>.json                  # static event/config fixtures
  <scenario>/node01/               # retained no-live-service node fixture
  <scenario>/node02/
  <scenario>/node03/

tests/fixtures/golden/mesh/
  <scenario>.<surface>.json.golden # stable JSON contracts

scripts/e2e_overhaul/
  mesh_<scenario>.sh               # shell e2e using J1/J3 logging

tests/
  mesh_<scenario>.rs               # RCH-friendly Cargo companion when needed
```

Node ids are always `node01`, `node02`, and `node03`. Use role labels only in
test descriptions (`primary`, `peer`, `relay`, `partitioned`); machine output
uses the stable node id. Workspaces created by shell e2e scripts live under
`$EPIC_WORKSPACE/mesh/<scenario>/<nodeId>/workspace`.

## Temp And Clock Rules

- Shell e2e scripts use `epic_setup` from
  `scripts/e2e_overhaul/lib/shared.sh`. Cargo integration tests that run on
  RCH workers use `/tmp` for temporary workspaces instead of inheriting a
  host-specific `TMPDIR`.
- Timestamps in fixtures are fixed RFC 3339 UTC values. Runtime logs may use
  the J1 logger timestamp, but assertions must not depend on wall-clock order
  beyond monotonic phase ordering within one log.
- Node ids, peer ids, workspace ids, event ids, and revision tokens are stable
  fixture strings unless the test is specifically about id generation.
- Tests must not require real Tailscale unless the bead is an explicit
  opt-in transport smoke test.

## Structured E2E Log Contract

All shell mesh e2e scripts source `scripts/e2e_overhaul/lib/shared.sh` and emit
`ee.test_event.v1` JSONL through `scripts/lib/e2e_logger.sh`.

Every script emits phases in this order:

1. `setup`
2. `action`
3. `assert`
4. `cleanup`

Use `mesh_phase_log <phase> <nodeId|scenario> <message>` for phase notes. The
helper stores these machine fields under `fields`:

```json
{
  "phase": "setup",
  "meshScenario": "mesh_off_no_network",
  "meshNode": "node01",
  "message": "node_workspace path=..."
}
```

Each script ends with a summary note containing:

- scenario name
- pass/fail assertion counters
- node count
- fixture root or retained workspace manifest

Raw command stdout, raw stderr, memory bodies, peer secrets, and full remote
workspace paths do not belong in mesh logs. Use hashes, fixture names, node ids,
redaction-safe aliases, and retained artifact paths.

## Scenario Helpers

`scripts/e2e_overhaul/lib/shared.sh` provides the common shell helpers:

- `mesh_scenario_setup <scenario> <node-count>` creates
  `$EPIC_WORKSPACE/mesh/<scenario>/nodeNN/{workspace,config,logs,goldens}` and
  emits `setup` phase rows.
- `mesh_node_workspace <nodeId>` prints the workspace path for a node and
  creates it if missing.
- `mesh_phase_log <phase> <nodeId|scenario> <message>` emits one structured
  `ee.test_event.v1` note with mesh phase fields.

Shell scripts may still call `ee_workspace` for single-workspace mesh-off
checks. Multi-node scripts call `mesh_node_workspace node01` and pass
`--workspace "$path"` explicitly.

## SRR6 Bead Mapping

| Bead | Required proof from this matrix |
| --- | --- |
| bd-x4hn7 | Mesh disabled by default; ordinary JSON is not polluted; no listener appears |
| bd-162sk | Byte-stability, no-network regression, and golden output parity |
| bd-3k16v | Replay convergence and partition/rejoin invariants |
| bd-3i5q7 | Privacy, redaction, body/embedding denial, and support-bundle leak checks |
| bd-3url9 | Latency, freshness, resource-budget, and cache-hit evidence |
| bd-ghey6 | Local two-node fixture without real Tailscale |
| bd-1crtj | Explicit opt-in real Tailscale smoke, quarantined outside normal verify |
| bd-3omr5 | Agent-facing command modes and JSON contracts |
| bd-2irom | Embedding/search-surrogate privacy and compatibility |
| bd-2vu8m | Final matrix audit proving every SRR6 shipped surface has unit proof plus e2e or golden proof |

New SRR6 test beads must reference this document in their description or first
tracker comment and must state which matrix rows they satisfy.

## Closeout Checklist

Before closing an SRR6 implementation bead, record:

- Unit or integration test command and result.
- E2E, golden, privacy, failure-mode, perf, or model-check evidence required by
  the bead mapping.
- Whether verification ran under RCH, local shell with an existing binary, or
  optional real transport smoke.
- The structured log path or artifact manifest path when a shell e2e ran.
- Any matrix rows intentionally deferred to a child bead.
