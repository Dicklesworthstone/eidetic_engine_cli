# ADR 0079: Read-only contention observability (`ee diag contention`)

Status: proposed
Date: 2026-06-16
Bead: bd-d67os.11 (epic bd-d67os — swarm hot-path concurrency)

## Context

Under massive agent swarms (dozens of concurrent `ee` processes plus internal
threads on 64-core / 256GB hosts) throughput is bottlenecked at a small set of
well-known hot-path locks and queues:

- the single write lock / write-owner queue (`src/db/mod.rs` `FileWriteOwnerGuard`;
  `src/core/write_owner.rs`),
- the read pool's acquire-wait and ad-hoc bypass under saturation
  (`src/db/read_pool.rs`),
- single-flight leader/follower coalescing posture (`src/core/singleflight.rs`),
- and — once Tracks B (group-commit, bd-d67os.1..4) and C (incremental index,
  bd-d67os.5..8) land — group-commit batch stats and index-intake stalls.

Each subsystem already exposes a cheap snapshot (`WriteOwner::status()`,
`ReadPool::stats()`, `singleflight_posture_report()`), but the signals are
scattered across five places. An operator or agent debugging a slow swarm has no
single read-only surface that answers "where is contention right now?". There
are ~30 `ee diag` subcommands but none aggregate lock-wait / saturation / stall.

## Decision

Add a read-only diagnostic, `ee.diag.contention.v1`, that aggregates the
existing per-subsystem snapshots into one posture report with a deterministic,
severity-ranked `topContention` list. This ADR + the report model + the
deterministic aggregation/posture/ranking collectors are bd-d67os.11; the
`ee diag contention` CLI command that gathers the live snapshots and emits the
envelope is bd-d67os.12.

The report:

- is **read-only** — it reads already-computed counters and never mutates any
  subsystem, opens no write transaction, and acquires no new lock;
- is **deterministic** given its inputs — the aggregation
  (`crate::core::contention::build_contention_report`) is a pure function of a
  `ContentionInputs` snapshot bundle, fully unit-testable with fixtures;
- embeds **no wall-clock** — only monotonic counters and measured wait
  nanoseconds, mirroring `ee.diag.plan_cache.v1` (ADR invariant: snapshot output
  excludes volatile timestamps so contract tests need no scrubbing);
- is **omit-safe** for future sources — `groupCommit`, `indexIntake`, and
  `l2Cache` are `Option` sub-reports omitted until Tracks B/C and an aggregate
  L2 accessor exist, so this leaf does not depend on them.

Three core sources (`writeLock`, `readPool`, `singleflight`) are always present
in the report; when a core source is genuinely unavailable at runtime (e.g. no
write-owner actor in one-shot CLI mode) it is reported with a zeroed section and
an entry in `unavailableSources`, not silently dropped.

## Posture model and thresholds (advisory v1)

Coarse severity `ok < warm < hot < contended`. `overallPosture` is the worst
across present sources. Thresholds (constants in `src/core/contention.rs`,
revisable as scale fixtures stabilize):

| Source | warm | hot | contended |
|---|---|---|---|
| write lock | queue ≥ 1 or any wait | queue ≥ 8 or maxWait ≥ 250 ms | queue ≥ 32, maxWait ≥ 2000 ms, or lockWait p99 ≥ 1000 ms |
| read pool | saturated, expired pins, release failures, or acquire p99 ≥ 1 ms | ad-hoc bypass > 0 or acquire p99 ≥ 50 ms | sizeWasZero, drops > 0, or acquire p99 ≥ 1 s |
| single-flight | active leaders or followers waiting (healthy coalescing) | follower timeouts or leader failures | state poisoned |

Single-flight coalescing is *healthy pressure relief*; only timeouts, failures,
and poisoning are adverse. Findings for posture ≥ warm carry a stable
`reasonCode` and copy-paste `suggestedCommands` (raise `EE_READ_POOL_SIZE`,
enable group-commit, route Cargo through RCH, etc.). `topContention` is sorted
severity-desc then source-asc for byte-stable output.

## Source gap codes

When a core runtime source is unavailable this run, `unavailableSources` carries
`{source, code}` with one of: `write_owner_unavailable`, `read_pool_unavailable`,
`singleflight_unavailable`. These are report-internal gap codes in this leaf.
They become first-class response-envelope `degraded[]` codes — each with a
`tests/fixtures/failure_modes/<code>.json` fixture and taxonomy classification —
when bd-d67os.12 lands the `ee diag contention` command that lifts them into the
envelope. Deferring the fixtures avoids orphan-fixture / unemitted-code gates
while there is no live emitter.

## Invariants

1. The collector never mutates any subsystem and never opens a write path.
2. `build_contention_report` is pure and deterministic; identical inputs produce
   byte-identical JSON.
3. The report embeds no wall-clock; all timing fields are measured counters.
4. `writeLock`, `readPool`, `singleflight` are always present; future-feature
   sub-reports are omitted (not zeroed) when absent.
5. Field names are camelCase and pinned by `docs/schemas/ee.diag.contention.v1.json`
   and the `tests/contracts/contention_schema.rs` structural contract.

## Rejected alternatives

1. **Extend each subsystem's own diag (`diag write-owner`, etc.) instead.** Keeps
   signals scattered; an operator still runs five commands and correlates by
   hand. The whole value here is the single aggregated, ranked view.
2. **Compute a live "contention score" with sampling/timers.** Would embed
   wall-clock and sampling noise, breaking determinism and golden tests. The
   existing monotonic counters are sufficient for a posture view.
3. **Mutating remediation (auto-raise pool size, auto-enable group-commit).**
   Out of scope and against the read-only diagnostic contract; remediation stays
   advisory via `suggestedCommands`.
4. **Block on Tracks B/C so the report is "complete".** Rejected: the core
   sources already justify the surface today, and the optional sub-reports are
   omit-safe, so D1 ships independently and the swarm gets value immediately.

## Consequences

- One read-only command (bd-d67os.12) will answer "where is the swarm
  contended?" with ranked, actionable findings.
- The `readPool` section directly motivates bd-d67os.15 (FIFO fairness) and the
  `writeLock` section motivates Track B (group-commit); the diagnostic and the
  fixes reinforce each other.
- Thresholds are advisory v1 and will be tuned against the bd-ppbue replay lab /
  bd-u7f9q admission fixtures without changing the wire contract.
- All Cargo verification for this leaf is RCH-only per repo policy.
