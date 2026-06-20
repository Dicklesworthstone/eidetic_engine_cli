# Contention Observability

`ee diag contention --json` is a **read-only** diagnostic that aggregates the
scattered hot-path contention signals an `ee` swarm hits under load — the
write-owner queue, the read pool, single-flight read coalescing, and (when
present) group-commit, incremental-index, and L2 pack-cache stats — into one
deterministic posture report with a severity-ranked `topContention` list. It
never mutates a subsystem, takes no new locks, and embeds no wall-clock; the
report is a pure function of the metric snapshots it reads (ADR 0079).

Use it to answer "where is the swarm contended right now, and what do I do about
it?" without scraping five separate commands.

## Severity ladder

Every source and the report overall carries a `posture` from one ordered enum:

| posture | meaning |
|---|---|
| `ok` | no meaningful contention observed |
| `warm` | mild pressure; informational (coalescing active, queue non-empty) |
| `hot` | notable pressure that adds latency under load |
| `contended` | saturation or failure signals; likely degrading throughput now |

`overallPosture` is the worst posture across all present sources.

## Sources

The report always carries the three core sources, then includes optional
sub-reports only when their telemetry is available (omit-safe — absent, never
`null`):

- **`writeLock`** — write-owner queue depth / wait, plus persisted
  `lockWaitMs` p50/p99 from the session-budget ledger. Hot/contended when
  concurrent writers serialize behind the single write lock.
- **`readPool`** — `db::read_pool` stats: active/idle, acquire-wait p99, ad-hoc
  bypass count, drops. `underized` is set when saturation indicators fire.
- **`singleflight`** — read-coalescing posture. Active leaders/followers are
  healthy pressure relief; follower timeouts, leader failures, or a poisoned
  state are the adverse signals. `coalesceEfficiency` is the fraction of
  would-be-duplicate computations avoided.
- **`groupCommit`** — daemon write-intake batching: `fsyncSaved`,
  `writesCoalesced`, `avgBatchSize`. In one-shot mode this is only the current
  process's counters; with `--use-daemon` it is the running daemon's live
  accumulated coalescing.
- **`indexIntake`** *(present once Track C is wired)* — index intake mode,
  rebuild count, and observed swap stalls.
- **`l2Cache`** *(present once an aggregate accessor exists)* — pack-cache
  `hitRate` and `thrashRatio`.

## `topContention` — the triage list

Each source whose posture reaches `warm` contributes one finding, ranked
**severity-descending, then source-ascending** (deterministic). A finding has a
stable machine `reasonCode`, a human `detail` line (no secrets/host paths), and
copy-paste `suggestedCommands` in priority order. Stable reason codes include:

| reasonCode | source | remediation hint |
|---|---|---|
| `write_lock_queue_backlog` / `write_lock_high_wait` / `write_lock_pressure` | writeLock | enable group-commit; route writers through the daemon write owner |
| `read_pool_ad_hoc_bypass` / `read_pool_high_acquire_wait` / `read_pool_saturated` / `read_pool_disabled` | readPool | raise `EE_READ_POOL_SIZE`; investigate long-held snapshot pins |
| `singleflight_follower_timeouts` / `singleflight_leader_failures` / `singleflight_state_poisoned` / `singleflight_active_coalescing` | singleflight | lower duplicate read pressure; restart to clear poisoned state |
| `group_commit_active_coalescing` | groupCommit | informational: daemon write batching is absorbing durable-write pressure |
| `index_swap_stalls` / `index_full_rebuild_amplification` | indexIntake | adopt incremental index intake |
| `l2_cache_thrash` | l2Cache | raise `EE_L2_PACK_CACHE_BYTES` or narrow the pack workload |

An agent triaging contention should read `topContention[0]` for the worst
bottleneck and apply its first `suggestedCommands` entry.

## `unavailableSources` — gaps, not failures

A core source that was expected but not gathered this run is reported as a gap
(not silently dropped). The gap codes are pinned against the schema's
`degradedEntry.code` enum:

- `write_owner_unavailable`
- `read_pool_unavailable`
- `singleflight_unavailable`

Optional sources (group-commit / incremental-index / L2) are simply omitted
when absent — they are not reported as gaps.

## One-shot vs daemon observability

This is the key operational nuance:

- **One-shot `ee diag contention`** reads *process-local* telemetry: this
  process's own (zeroed) group-commit atomics and an idle single-flight
  registry. The write-owner queue and read-pool stats need a live actor/pool
  handle a one-shot CLI does not have, so they appear in `unavailableSources`.
  A fresh one-shot report on an otherwise-busy host is therefore typically
  `overallPosture: ok` with gaps — that is expected, not a bug.
- **`ee diag contention --use-daemon`** queries the running daemon over its
  socket (`ee.daemon.telemetry`). The daemon process accumulates *real*
  coalescing across the writes it services, so its group-commit and
  single-flight counters reflect genuine cross-request contention that a
  one-shot snapshot cannot see. This is the live posture readout.

## Daemon write durability contract

`ee.daemon.write` and `ee.daemon.write_journal` success means the daemon write
owner has returned from the database transaction for that write. The database is
the durable source of truth; search and derived indexes may lag and are
rebuildable. Under the current WAL `synchronous=NORMAL` policy this is the
normal SQLite committed-state contract for process/app crashes, not a promise
that a fresh OS or power-loss checkpoint has already happened.

The shipped `[write].group_commit_enabled` default remains `false`. The daemon
write actor uses a bounded internal group-commit path for daemon-routed writes,
while the global config default stays gated behind the RCH soak/perf proof
before any broader default-on flip.

If `--use-daemon` cannot reach the daemon, the command **degrades gracefully**:
it falls back to the in-process snapshot, still exits 0, still emits a valid
report, and records a `degraded` entry (`daemon_socket_unavailable`, repair:
`ee daemon start`). Internal serialization failures surface as
`daemon_telemetry_encode_failed`.

## Examples

```bash
# Process-local snapshot (read-only, no daemon required).
ee diag contention --workspace . --json

# Live posture from the running daemon's accumulated coalescing.
ee diag contention --workspace . --use-daemon --json
```

The JSON envelope is `ee.response.v2` with the report under `data.report`:

```json
{
  "schema": "ee.response.v2",
  "success": true,
  "data": {
    "command": "diag contention",
    "report": {
      "schemaTag": "ee.diag.contention.v1",
      "overallPosture": "contended",
      "writeLock": { "queueDepth": 40, "posture": "contended", "...": "..." },
      "readPool": { "adHocBypassCount": 5, "posture": "contended", "...": "..." },
      "singleflight": { "coalesceEfficiency": 0.75, "posture": "hot", "...": "..." },
      "topContention": [
        {
          "source": "read_pool",
          "severity": "contended",
          "reasonCode": "read_pool_ad_hoc_bypass",
          "detail": "read pool 8/8 active, 5 ad-hoc bypasses, ...",
          "suggestedCommands": ["raise EE_READ_POOL_SIZE for this workload", "..."]
        }
      ],
      "unavailableSources": []
    }
  },
  "degraded": []
}
```

## References

- Schema: [`docs/schemas/ee.diag.contention.v1.json`](../schemas/ee.diag.contention.v1.json)
- Design: [`docs/adr/0079-contention-observability.md`](../adr/0079-contention-observability.md)
- Collector core + unit tests: `src/core/contention.rs`
- Deterministic goldens: `tests/fixtures/golden/contention/*.json`
- Structural contract: `tests/contracts/contention_schema.rs`
- Real-binary E2E: `scripts/e2e_diag_contention.sh`
