# ADR 0013: Single-Write-Owner Actor

Status: accepted
Date: 2026-05-05

## Context

SQLite allows only one writer at a time. When multiple `ee` processes or threads
attempt concurrent writes, they contend for the write lock. This causes:

1. SQLITE_BUSY errors that surface as confusing storage failures.
2. Non-deterministic write ordering when multiple agents push memories.
3. Risk of partial writes if a process dies mid-transaction.

The franken-stack (FrankenSQLite via SQLModel) handles connection pooling and
WAL mode, but does not coordinate multiple processes attempting writes.

## Decision

**All durable writes flow through a single-writer actor.**

1. A `WriteOwner` actor (in `src/core/write_owner.rs`) holds exclusive write
   capability for the database.
2. Write requests are submitted to a channel; the actor processes them serially.
3. Read operations bypass the actor and go directly to the read pool.
4. CLI commands that write (`remember`, `import`, `curate apply`) acquire a
   write slot from the actor before proceeding.
5. If another writer is active, the command either waits (with timeout) or
   returns a `write_owner_busy` error with repair guidance.

## Consequences

- No SQLITE_BUSY races between concurrent `ee` invocations.
- Write ordering is deterministic: first-in-first-out through the channel.
- Agents can check write availability before committing to a mutation.
- Long-running imports hold the write slot; other commands see backpressure.
- The actor is optional in single-threaded CLI mode but mandatory for daemon/serve.

## SRR3 Write-Hot-Path Invariant Manifest

The SRR3 write-hot-path work keeps the single-write-owner contract but adds a
model for queued writes, batch boundaries, cancellation, fsync failure, recovery,
and snapshot publication. The executable reference model is the `WriteSpool`
surface in `src/core/write_owner.rs`; it is pure, deterministic, and isolated
from database mutation.

| Invariant | Contract | Executable evidence |
| --- | --- | --- |
| Deterministic schedule input | The model is driven by explicit enqueue and cancellation events, not wall-clock arrival races. | `write_spool_group_commit_preserves_order_audit_and_snapshot_invariants` builds generated schedules from `ScheduledSpoolWrite`. |
| Per-producer FIFO | Writes from the same producer keep increasing request IDs and may not reorder within durable batches. | `assert_write_spool_schedule_invariants` records per-producer request IDs and asserts adjacent IDs increase. |
| Group-commit boundary determinism | Batch IDs, job row IDs, audit row IDs, kind, durability, and request order are reproducible from the schedule. | `write_spool_batches_eligible_writes_and_isolates_immediate_imports` and the property invariant assert stable batch metadata. |
| Cancellation before commit | A cancelled pending request is retained in recovery state as cancelled and excluded from durable batches. | `write_spool_lab_runtime_cancellation_is_recoverable` and the property invariant assert cancelled records never enter a batch. |
| Fsync failure stops publication | A failed batch records a structured failure and does not advance snapshot generation. | `write_spool_invariant_fsync_failure_propagation_model` and the property invariant simulate fsync failure before snapshot advancement. |
| Audit-chain continuity | Emitted batch IDs must be contiguous, with no holes in the modeled audit chain. | `assert_write_spool_schedule_invariants` checks every batch ID from `1..=max` exists. |
| Snapshot publication monotonicity | Snapshot generation advances only after committed batches and remains monotone. | `write_spool_invariant_snapshot_generation_monotone` and the property invariant check committed-batch advancement. |
| Recovery preserves outcomes | Replay reconstructs pending, committed, cancelled, and failed rows without inventing durable rows. | `write_spool_recovery_distinguishes_pending_committed_cancelled_failed`. |
| Backpressure is explicit | Queue depth, pending bytes, and queue age failure modes return structured backpressure instead of silent drops. | `write_spool_backpressure_reports_json_contract`, `write_spool_lab_runtime_queue_timeout_backpressure`, and `write_spool_lab_runtime_pending_bytes_backpressure`. |

The manifest is intentionally pre-implementation: SRR3 production queue work must
extend these invariants rather than weaken them. The downstream SRR3.spec beads
own broader property generators, failure-mode fixtures, fake-runner coverage, and
implementation-gate updates.

## Rejected Alternatives

- **Rely on SQLite WAL alone**: WAL improves concurrency but does not eliminate
  writer contention across processes.
- **Distributed lock file**: Adds filesystem state outside the database; harder
  to reason about and recover from.
- **Optimistic locking with retries**: Non-deterministic; agents cannot predict
  which write wins.

## Verification

- `tests/contracts/single_writer_stress.rs`: Two competing writers; one waits,
  no BUSY errors.
- `ee diag locks --json`: Reports current write owner, queue depth, wait time.
- CLI integration tests verify `write_owner_busy` error shape and exit code.
