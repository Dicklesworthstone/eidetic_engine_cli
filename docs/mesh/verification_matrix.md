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

## SRR6.20 Two-Tier Budgets

`tests/mesh_two_tier_budget.rs` is the RCH-friendly structured proof for
`bd-3url9`. It exercises the pure `ee.mesh.two_tier_budget.v1` report and
records these default foreground budgets:

| Budget | Default |
| --- | --- |
| Tier 1 local answer p50 | 75 ms |
| Tier 1 local answer p99 | 250 ms |
| Async peer probe timeout | 750 ms |
| Stale-read window under normal sync cadence | 5000 ms |
| Peer freshness fanout | 32 peers |
| Lazy body-cache growth per foreground read | 512 KiB |
| Sync batch size | 512 events |
| Index-job amplification per round | 16 jobs |

The report must keep `localAnswerBlocking=false`, `networkOnTier1=false`, and
`bodyTransferAllowedOnTier1=false`. Cache-hit evidence is reported explicitly
through `cacheHitPathObserved`; missing cache-hit evidence is treated as a
budget degradation for the SRR6.20 proof row.

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
- The opt-in real transport smoke is
  `scripts/e2e_overhaul/mesh_tailscale_smoke.sh`. It exits `78` by default and
  only runs against a live tailnet when `EE_E2E_REAL_TAILSCALE=1` and
  `EE_REAL_TAILSCALE_PEER` are both set.

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
| bd-1crtj | Explicit opt-in real Tailscale smoke via `scripts/e2e_overhaul/mesh_tailscale_smoke.sh`, quarantined outside normal verify |
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

Before closing `bd-2vu8m`, record the final rollup in the bead thread:

- `br dep cycles --json --no-db` result and the command timestamp.
- `scripts/closeout_audit.sh --bead bd-2vu8m --json` readiness, blockers,
  caveats, and artifact path when retained.
- Every SRR6 child bead that is not closed, with explicit status, deferral
  rationale, owner, and the proof row that remains unverified.
- Mesh-disabled proof status for
  `scripts/e2e_overhaul/mesh_off_no_network.sh`, `tests/mesh_off_no_network.rs`,
  and `tests/fixtures/golden/mesh/mesh_off_no_network.commands.json.golden`.
- RCH posture for every Cargo command. If RCH refuses remote execution or reports
  local fallback, record the reason and keep the Cargo proof gate unverified.

## Team confederation closeout (ADR 0086 / T6.7)

Campaign: `bd-tc-epic-qzk7o`. Proof host: isolated
`ubuntu@38.242.134.66` `/tmp/ee-mesh-verify` (no Mac local Cargo).
This is a live Unix EE-to-EE ledger, not a bead-closing ceremony.

| Slice | Status | Proof | Remainder |
| --- | --- | --- | --- |
| T1–T4 transport, origin, join, authorizer, pair-key, leave/pause | **Proven** | Isolated live TCP hello, sync_round, join, revoke, history share, signed inbound persist. Create/join raise the invite-authorization floor. `ee team projects reconcile` rematerializes origin `teamProjectShared` rows (V116) whose `projectId` is `prj_tm_` plus 26 chars. Isolated 2026-08-14: `reconcile_rematerializes_origin_project_shares` 60.95s exit 0; `resume_pending_invite_omits_the_secret` 51.71s exit 0. | A 29-char fixture id is ignored by reconcile; doctor now uses the same id check |
| T5.1–T5.8 pack scope, activity, attribution | **Landed** | Product commands + unit tests | Full US-6 crash-injection matrix is not a separate harness |
| T5.9 Unix body product | **Proven** | `share_team_bodies_publishes_then_unshare_stops_serving` exit 0; live BodyFetch; confirm gated on secure-file | Files never deleted; reconcile does not resurrect |
| T5.9 Windows SID/DACL/reparse | **Adapter compiles (`HardenedWindows`)** | Isolated `cargo check --target x86_64-pc-windows-gnu --lib` Finished in 9m 44s after enabling `windows_by_handle`, gating Unix-only responder control, and stubbing the Windows daemon search type. Adapter rejects reparse points, pins file identity, applies/verifies protected owner+SYSTEM DACL, write-through publish. | No Windows-host runtime DACL soak; inbound responder remains Unix-only. |
| T5.10 US-6 E2E | **Proven (Unix product harness)** | Token bind/drift/expiry/wrong-store, unlinkable previews, crash lifecycle. Isolated 2026-08-14: `already_redacted` 58.60s, `body_lane_grant_then_revoke_gates_fetch` 61.08s, `inspect_team_health` origin_outbox 52.54s, `substituted_body_cache_bytes_stay_metadata_only` 54.58s. `ee team share bodies --representation`; hash-checked fetch | No Windows-host crash killer |
| T6.1 steward | **Proven** | Isolated 2026-08-14: membership execute 51.59s; `snapshot_from_paths_loads_peers_and_sync_once_stays_deferred_when_mesh_off` 107.15s. Daemon calls `run_mesh_sync_once_from_paths` when `ran_sync`. Execute also rematerializes origin project shares. | Live contact still needs mesh enabled + reachable peers |
| T6.2 daemon install | **Proven on Linux and macOS user supervisors** | Linux: `systemctl --user start ee-team-confed-proof.service` → `ActiveState=active` at 2026-08-14T01:47:44Z. macOS: `launchctl bootstrap gui/501/ai.eideticengine.ee-team-confed-proof` → `active count = 1` / `state = xpcproxy` at 2026-08-14T02:14:58Z. Both units quarantined by rename. | Real `ee` binary KeepAlive service was not left running; Windows remains client-only |
| T6.3 admission | **Proven** | Authenticated serve folds decisions into a broker-owned per-peer map and persists a V118 snapshot. Isolated 2026-08-14: `persisted_admission_snapshot_warns_doctor_and_status` 56.71s exit 0; inspect 56.12s exit 0. Status/doctor report throttled/exhausted counts and coalesced exhaustion after the broker exits. | Windows inbound listen remains out of scope |
| T6.4 doctor | **Proven** | Isolated 2026-08-14: floor 46.91s, inspect 50.86s, removal rematerialize 50.80s, `removal_acknowledgement_matrix_stays_pending_until_audience_applies` 55.88s, all exit 0. V117 persists the removal audience; doctor warns on pending acks and does not claim bounded fanout. Steward advances acks from peer cursors. | Windows inbound listen remains out of scope |
| T6.5 budgets | **Proven** | `ee team status` emits `budgets` (`ee.team.budgets.v1`) naming join event-batch count, signed-relay batch bytes, body fetch bytes, and index jobs/round. Isolated 2026-08-14: `team_confed_budget_profile_names_join_relay_body_and_index_caps` 53.51s exit 0. At-cap EventBatch is allowed; +1 is rejected with `local_tier1_unaffected`. | Criterion `[[bench]]` remains opt-in; compile cost dominates |
| T6.6 docs | **Landed** | `docs/team/quickstart.md`, `trusted_vs_contractor.md`, `docs/agent-ux/team.md`, CHANGELOG | — |
| T7.1–T7.6 IdP | **Proven** | Fake-IdP HTTPS RS256 1.40s; live TCP identity_attest 88.42s compile+test | No production IdP vendor soak |
| T7 Windows / client-only | **Fail-closed** | Same as T5.9 | — |

Campaign is **done as live Unix EE-to-EE capability**, including
T5.10, T6.2, T6.3 V118 admission snapshots, T6.4 invite-floor /
V117 removal acks, and T6.5 `ee.team.budgets.v1` on
`ee team status`. Isolated 2026-08-14T16:03–16:34Z: budget
profile 53.51s exit 0. Windows inbound listen and a Windows-host
DACL soak remain out of scope. Do not self-close beads from this
ledger.
