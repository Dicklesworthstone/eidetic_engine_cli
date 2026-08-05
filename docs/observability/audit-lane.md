# Audit Lane

`ee.audit_lane.v1` is the telemetry contract for the Swarm-X audit lane
planned under `bd-wp5ac`. The lane moves high-volume audit emission off the
foreground mutation path without weakening the audit hash chain.

This page documents the contract for the implementation slices. The bounded
queue, batch sink, and conservative `ee remember` foreground fallback are wired;
the process-wide lane lifecycle remains a later integration step.

## Event Contract

Every audit-lane telemetry event has:

| Field | Meaning |
| --- | --- |
| `schema` | Always `ee.audit_lane.v1`. |
| `phase` | One of `enqueue`, `drain`, `batch_commit`, `shutdown`, or `backpressure`. |
| `workspaceId` | Workspace whose audit stream is affected. |
| `requestId` | Caller request identifier when available; otherwise `null`. |
| `auditSeq` | Per-workspace sequence assigned before enqueue. `0` means no event was accepted. |
| `batchSize` | Number of events drained or committed for batch phases. |
| `elapsedMs` | Wall-clock duration for the phase. |
| `degradedCodes` | `audit_backpressure`, `audit_lane_batch_commit_failed`, and/or `audit_lane_shutdown_drain_timeout`. |

## Ordering

The lane preserves successful enqueue order and commits batches without
reordering; it does not sort events by `auditSeq`. Writer integration must
therefore serialize each workspace's enqueues in assigned `auditSeq` order when
numeric sequence order is required. Later slices must keep the same inputs in
the same enqueue order byte-stable: the same sequence of committed durable
mutations must produce the same `auditSeq` order.

## Chain Hash Continuity

The existing `audit_log` hash chain remains authoritative. Batched writes must
compute the first row's `prev_row_hash` from the latest committed audit row and
then thread each subsequent row's `prev_row_hash` through the prior row in the
same batch. A batch is invalid if any row would skip or fork the chain.

## Backpressure

When the producer queue is full, the foreground operation must receive an
`audit_backpressure` degradation. The event must not be silently dropped. The
foreground durable mutation may continue only if the implementation can either
enqueue the audit event later or explicitly report that audit durability is
degraded for that request.

## Foreground Fallback

Until the audit lane is enabled for a call site, foreground operations keep the
existing direct `audit_log` insert behavior. Integration code should route
events through `emit_with_direct_fallback`: when no lane handle is configured it
executes the direct insert path; when enqueue succeeds it skips the direct path;
when the lane is full it reports `audit_backpressure` and executes the direct
insert path; when the lane is closed it executes the direct insert path without
claiming queue durability.

This fallback is deliberately conservative. Enabling the lane must be
byte-stable for ordinary responses when the queue accepts events, and disabled
lane behavior must remain identical to the pre-lane direct insert path.

The current `ee remember` call site routes its memory-create and policy-bypass
audit rows through this helper. Public CLI execution still passes no lane handle,
so it keeps the pre-lane direct `audit_log` insert behavior. Unit coverage may
inject a lane handle to prove the enqueue path and then drain the resulting
events into `insert_audit_batch`.

## Shutdown

Shutdown drains the queue and performs a final batch commit. If the drain budget
expires, the lane emits `audit_lane_shutdown_drain_timeout` with the number of
events not yet committed. Tests and e2e artifacts must retain enough evidence
for `ee audit verify --json` to prove whether the durable chain is complete.

If a batch sink returns an error, the fallible drain path returns that source
error with `audit_lane_batch_commit_failed`, `failedEvents`, and
`failedBatches` in the report. The error also owns the failed batch as
`undelivered`, preserving its original FIFO order. `failedEvents` is always the
length of that vector, while `pendingEvents` counts only events that remain
owned by the lane and queued for a later drain. It excludes events transferred
to the caller through `undelivered`.

Callers must recover or retry `undelivered` before permitting any later audit
commit, including another lane drain or a backpressure/closed-lane direct
fallback; otherwise durable audit order can diverge from acceptance order. The
lane does not coordinate a returned drain error with producer-side direct
fallbacks, so writer integration must serialize that recovery. A fallible batch
sink must therefore be all-or-nothing on error, or support an idempotent retry
of the returned batch. The current database batch sink is transactional.
Callers must not count a failed batch as drained or claim audit durability for
the affected mutations until that recovery succeeds.

## Crash Safety

The in-memory queue is not a durable log. Crash recovery relies on the invariant
that committed durable mutations and their audit rows are committed in a safe
order by the writer. The implementation must not claim a mutation is fully
audited until the corresponding audit batch has committed or the response
contains a degraded code explaining the audit gap.
