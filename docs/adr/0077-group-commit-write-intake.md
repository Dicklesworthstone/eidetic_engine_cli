# ADR 0077: Group-Commit Write Intake

Status: accepted
Date: 2026-06-16
Bead: bd-d67os.1

## Context

Under a massive agent swarm (dozens of concurrent `ee` processes on a
64-core / 256 GB host), the binding throughput constraint on the durable-write
path is the single-writer WAL wall. Every durable write — `ee remember`,
`ee outcome`, `ee journal append`, and the daemon write-owner flows — takes the
per-DB-file write lock `FileWriteOwnerGuard` (`src/db/mod.rs:453-530`),
and opens its own transaction (`src/db/mod.rs:986`, `:1018`; WAL
`synchronous=NORMAL` at `:1709`). Under WAL `synchronous=NORMAL`, most commits
do not issue a power-loss fsync; the hot-path win is fewer write-lock
acquisitions, WAL commit boundaries, and commit round-trips. N concurrent
writers therefore serialize into N transaction boundaries even when they arrive
within microseconds of each other and could have shared one commit.

The mechanism to fix this is already present but inert:

- `WriteHotPathConfig { enabled: false, .. }` (`src/core/write_owner.rs:1777`,
  default at `:1792`) already carries `group_commit_max_rows` and
  `group_commit_max_us` knobs.
- `drain_group_commit()` exists but is never invoked
  (`src/core/write_owner.rs:1883`).
- `WriteSpoolBatch` today batches **audit metadata only**, not the durable
  transaction boundary itself.

So the infrastructure is defined and disabled. What is missing is a documented
contract for how concurrent in-flight writes coalesce, a configuration surface
to enable and bound it, and a telemetry schema to prove the win and operate the
fallback. This ADR fixes that contract. It is the foundation leaf
(`bd-d67os.1`) of Track B (group-commit) under the hot-path concurrency epic
`bd-d67os`; the core, integration, and enable-flip land in `bd-d67os.2`,
`bd-d67os.3`, and `bd-d67os.4`.

This ADR records a contract only. It does not change runtime behavior:
group-commit ships disabled (`group_commit_enabled = false`) until the
`bd-d67os.4` proof flips it.

## Decision

**Group-commit write intake** coalesces concurrent in-flight durable write
requests that arrive within a bounded batch window into **one transaction and
one `fsync`**, while preserving every write's own audit row, idempotency
semantics, and per-write result.

The intake sits inside the existing write-owner critical section. It does not
introduce a second writer, a background runtime, or a new command. It is an
internal mechanism beneath the existing `durable_write` / `AuditedMutation`
effect classification.

### Coalescing model

| Property | Contract |
| --- | --- |
| Batch trigger | A batch closes when the first of `batch_window_ms` elapses, `max_batch_size` requests accumulate, or `max_inflight_bytes` of pending payload is reached — whichever comes first. |
| Atomicity | All writes in a closed batch commit in ONE transaction with ONE `fsync`. A batch either commits whole or rolls back whole; partial visibility is never observable. |
| Audit preservation | Each coalesced write still emits its own audit row(s) with its own content hash chain. Coalescing changes the commit boundary, never the audit cardinality. |
| Idempotency | Per-write idempotency keys are evaluated independently inside the batch; a duplicate within a batch resolves to the same no-op it would have under per-write commit. |
| Ordering | Writes commit in arrival order within the batch; cross-batch ordering is the arrival order of batch closure. Determinism for a given input sequence is preserved. |
| Fail-safe | When disabled, degraded, or when a single oversized write exceeds `max_inflight_bytes`, intake falls back to the existing per-write transaction path. Fallback is always available and is the default. |

### Configuration surface

A `[write]` configuration table (env overrides `EE_WRITE_GROUP_COMMIT_*`,
registered in `src/config/env_registry.rs` and documented in
`docs/env_vars.md`):

| Key | Default | Meaning |
| --- | --- | --- |
| `group_commit_enabled` | `false` | Master switch. Stays false until the `bd-d67os.4` perf proof flips it. |
| `batch_window_ms` | bounded small (e.g. `2`) | Maximum time the first writer waits for peers before the batch closes. |
| `max_batch_size` | bounded (e.g. `64`) | Maximum number of writes coalesced into one commit. |
| `max_inflight_bytes` | bounded | Backpressure ceiling on pending coalesced payload; a write larger than this commits alone. |

The configuration must fail-safe to the per-write path when disabled or when any
bound is unset or invalid. No bound may be unbounded.

Default-on remains gated: this ADR keeps the shipped `[write]` default disabled
until the RCH soak/perf proof for `bd-d67os.4` closes. The daemon-hosted write
actor may use its bounded internal coalescing path for daemon-routed writes, but
that does not flip the global one-shot/default CLI path.

Success from `ee.daemon.write` / `ee.daemon.write_journal` means the daemon
write owner returned from the database transaction for that write. The database
is the durable source of truth; derived search/index assets may lag and remain
rebuildable. With the current WAL `synchronous=NORMAL` setting, the ACK is the
normal SQLite committed-state contract for process/app crashes rather than a
fresh checkpoint or power-loss fsync guarantee.

### Telemetry schema

`ee.write_group_commit.v1` (category `performance`, redaction-safe — counts and
latencies only, never write payloads) is the normative telemetry contract:

| Field | Meaning |
| --- | --- |
| `batches` | Count of closed batches. |
| `writes_coalesced` | Count of writes that shared a batch with at least one peer. |
| `avg_batch_size` | Mean writes per batch. |
| `fsync_count` | `fsync`s actually issued. |
| `fsync_saved` | `fsync`s avoided versus the per-write baseline (`writes - fsync_count`). |
| `commit_latency_p50_us` / `commit_latency_p99_us` | Commit latency distribution. |
| `fallback_count` | Writes that took the per-write fallback path. |
| `fallback_reason` | Closed-set reason for fallback (`disabled`, `degraded`, `oversized`, `single_writer`). |

The schema registers in `public_schemas()` (`src/output/mod.rs`), the
`schema_list` golden (`tests/fixtures/golden/schema/schema_list_json.golden`),
and the `docs/schemas/ee.write_group_commit.v1.json` document. Both the
registration and the golden must be updated together — those gates are routinely
left half-updated.

## Relationship To Existing Work

- **Resource admission** (`bd-u7f9q`) governs whether a workload is admitted;
  group-commit governs how admitted writes commit. They compose: admission
  shapes arrival rate, intake shapes commit batching.
- **Scale envelope** (`ee.scale_envelope.v1`, `bd-ssoco`) reports write-spool
  state as posture. Group-commit telemetry is the per-mechanism counterpart that
  proves the write-side SLO, and the scale envelope may later cite it.
- **Incremental index intake** (Track C, `bd-d67os.5`) attacks the
  full-rebuild-per-write amplification. Group-commit and incremental intake are
  independent and complementary: one reduces durable transaction boundaries, the
  other reduces index work, both on the same hot write path.
- **Audit chain** is unchanged in cardinality and content. Group-commit moves
  only the transaction boundary; `insert_audit` still produces one row per write
  (see the caller-held-transaction handling at `7a28413d`).

## Constraints

- Franken-stack only: no `tokio`, `rusqlite`, or `petgraph`. The batch window is
  a bounded synchronous wait inside the write-owner critical section, not an
  async runtime; runtime-facing async takes `&Cx` and returns `Outcome<T>` with
  budget and cancellation preserved. `#![forbid(unsafe_code)]` holds.
- Determinism: for a given input sequence and configuration, JSON output and
  pack hashes are stable. Batch boundaries may vary with timing, but committed
  state and audit content do not.
- No silent memory mutation: every coalesced write remains audited. Coalescing
  is observable only through `ee.write_group_commit.v1` counters, never through
  changed or missing audit rows.
- Single-process MVCC only: group-commit never introduces a second OS-level
  writer to the same database file.
- Local Cargo is not part of the verification contract on this Mac swarm lane;
  the config and schema unit tests are proven RCH-only per the epic constraint.

## Rejected Alternatives

- **Per-write as-is:** rejected because it is precisely the single-writer WAL
  wall this epic exists to remove; N concurrent writers pay N `fsync`s.
- **Multi-writer database:** rejected — forbidden by the single-process MVCC
  constraint. Concurrent OS-level writers to one FrankenSQLite file are not a
  correctness model `ee` relies on.
- **Async (tokio) commit batching:** rejected — `tokio` is a forbidden
  dependency, and a background async committer would fracture the audit-chain
  atomicity guarantee and the deterministic commit-ordering contract.
- **Always-on coalescing (no enable flag):** rejected — the win must be proven
  on the perf lane (`bd-d67os.4`) before flipping; shipping enabled-by-default
  without a measured fallback path would risk latency regressions on
  low-concurrency workloads.
- **Unbounded batch window or size:** rejected — every bound must be finite so a
  lone writer never waits indefinitely and backpressure stays predictable for
  swarm agents.

## Verification

- A contract test (mirroring `tests/contracts/scale_envelope_schema.rs`) pins
  `ee.write_group_commit.v1` schema identity, `public_schemas()` registration,
  `schema_list` golden membership, the required telemetry fields, and the closed
  `fallback_reason` set.
- Config unit tests assert `[write]` defaults (group-commit disabled), bounded
  ranges, `EE_WRITE_GROUP_COMMIT_*` env-override parsing, and fail-safe-to-
  per-write when disabled or invalid.
- A `tests/fixtures/failure_modes/*` fixture documents the `fallback_reason`
  vocabulary with trigger shape and the per-write fallback as the safe path.
- Later Track B beads (`bd-d67os.2`, `.3`, `.4`) implement and route the intake
  and emit this schema rather than introducing new write-side telemetry fields;
  `bd-d67os.4` carries the no-mock `e2e_group_commit.sh` proof and the
  enabled-flag flip.
- All Cargo verification for the config and schema is RCH-only on this Mac lane.
