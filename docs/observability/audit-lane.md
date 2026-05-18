# Audit Lane

`ee.audit_lane.v1` is the telemetry contract for the Swarm-X audit lane
planned under `bd-wp5ac`. The lane moves high-volume audit emission off the
foreground mutation path without weakening the audit hash chain.

This page documents the contract for the implementation slices that follow.
It does not claim the runtime queue is already wired.

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
| `degradedCodes` | `audit_backpressure` and/or `audit_lane_shutdown_drain_timeout`. |

## Ordering

The lane preserves total ordering per workspace. Producers assign or receive a
per-workspace `auditSeq` before enqueue; the single writer drains by that order
and commits batches without reordering. Later slices must keep same inputs in
the same order byte-stable: the same sequence of committed durable mutations
must produce the same `auditSeq` order.

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

## Shutdown

Shutdown drains the queue and performs a final batch commit. If the drain budget
expires, the lane emits `audit_lane_shutdown_drain_timeout` with the number of
events not yet committed. Tests and e2e artifacts must retain enough evidence
for `ee audit verify --json` to prove whether the durable chain is complete.

## Crash Safety

The in-memory queue is not a durable log. Crash recovery relies on the invariant
that committed durable mutations and their audit rows are committed in a safe
order by the writer. The implementation must not claim a mutation is fully
audited until the corresponding audit batch has committed or the response
contains a degraded code explaining the audit gap.
