# Mesh Anti-Entropy Protocol

> Bead: `bd-3lx0p` (SRR6.7). ADR: [`0041-mesh-anti-entropy-model.md`](../adr/0041-mesh-anti-entropy-model.md).
> Sync-summary schema: [`ee.mesh.anti_entropy.v1.json`](../schemas/ee.mesh.anti_entropy.v1.json).

Mesh anti-entropy reconciles two peers' append-only event streams after
partitions, restarts, duplicate deliveries, and out-of-order arrival. It is
the smallest mechanic that makes ADR 0037's "local-first cache + explicit
revision channel" honest under realistic network failures. It is not gossip,
not consensus, and not CRDT merging.

This document is the wire-protocol contract. The pure invariants are in
ADR 0041; the executable model lives in `src/mesh/anti_entropy_model.rs`.

## Message kinds

A single anti-entropy round between peer `A` and peer `B` consists of four
deterministic message kinds. Every kind passes through the same SRR6.5
peer-policy gates as a local import — anti-entropy never creates a side
channel that bypasses authorization.

### 1. `TipAdvertise { frontier }`

Sender publishes its current contiguous-replay frontier as
`BTreeMap<origin_node_id, last_contiguous_seq>`. The frontier is what the
sender considers safely importable from itself: any seq ≤ the advertised
tip has been durably accepted.

A `TipAdvertise` carrying a non-contiguous frontier (a tip for an origin
whose prior seqs were not all accepted) is a protocol error. Recipients
reject it without queueing a retry and emit a stable degraded code.

### 2. `RangeRequest { origin_node_id, start_seq, end_seq }`

Receiver compares its own frontier against an advertised peer frontier and
emits zero or more bounded range requests. Each request covers exactly one
origin and one contiguous span starting at `local_cursor + 1`. A single
anti-entropy round emits at most one range request per origin — the
`ranges_to_request(...)` helper on the model enforces this.

If the peer cannot serve the full range (it has been tombstoned, exceeds
the per-message size budget, or fails policy gating), the response
narrows the range and the requester re-asks in the next round. Range
amplification (a request spanning multiple origins, or unbounded spans) is
a protocol error.

### 3. `EventBatch { origin_node_id, events: [...] }`

Sender returns the contiguous events for a requested range, in order by
`seq`. Each event still carries its full `(origin_node_id, seq, event_id,
logical_memory_id, base_event_id, content_hash)` shape so the receiver can
replay it idempotently without trusting any positional information.

A batch that includes a hole (e.g. `seq=5` then `seq=7` with no `seq=6`)
is rejected as a protocol error. The receiver does not skip the hole.

### 4. `RevisionNotice { advanced_origins: [...] }`

When the receiver's frontier advances after replay, it emits a revision
notice listing every `(origin_node_id, from_seq, to_seq)` advance since
the previous Tier 1 read snapshot. This is the only signal that lets a
caller learn about new peer evidence after a context pack has already been
returned. Receipt of a `RevisionNotice` never mutates the previously
returned pack — it is purely informational.

## Round shape

A round between `A` (initiator) and `B`:

```
A ──▶ TipAdvertise(frontier_A)            B
A     ◀── TipAdvertise(frontier_B)        B
A: ranges = ranges_to_request(frontier_B) compared against accepted
A ──▶ RangeRequest(origin=X, start, end)  B
A     ◀── EventBatch(origin=X, [...])     B  (one batch per accepted request)
A: replay each event in batch; idempotent; fork-rejecting
A: if frontier advanced, emit RevisionNotice locally to subscribers
```

Either side may initiate; the protocol is symmetric. There is no
follow-up acknowledgement message — durable acceptance is reflected in the
next round's `TipAdvertise`.

## Bounded retry/backoff

Range requests use bounded retry with exponential backoff and an explicit
`retry_after` budget. The supervisor that schedules rounds (`bd-1ylr3`
SRR6.10) is responsible for the wall-clock budget; this protocol commits
only to the per-request shape:

- Initial backoff: 1 second.
- Multiplier: 2x per failure.
- Cap: 60 seconds.
- Max attempts per range, per peer: 5.

After max attempts the range is recorded as
`anti_entropy_range_blocked` in the sync summary with a stable
`retry_after` timestamp; no further attempts are made until either the
supervisor's next budgeted round or an operator-triggered refresh.

Unbounded retry violates ADR 0037 ("no daemon-required core workflow") and
ADR 0041 ("Bounded retry/backoff is a hard invariant"). New supervisor
work that needs different bounds requires an ADR amendment.

## Partition and rejoin

After a partition, the rejoin round is no different from any other round:
both sides exchange `TipAdvertise`, the lagging side computes the
`ranges_to_request` that close the gap, and the catching-up side replays
the resulting `EventBatch` events idempotently. Duplicates are absorbed
as `Duplicate` outcomes. Out-of-order delivery (a peer sending `seq=7`
before `seq=6`) does not occur in `EventBatch` (rejected as a protocol
error) but if the supervisor inadvertently merges two batches, the
replayer still preserves the contiguous-cursor invariant: `seq=7` is
accepted but the frontier does not advance past `seq=5` until `seq=6`
arrives.

## Forks and logical conflicts

Two distinct event-IDs at the same `(origin_node_id, seq)` are a fork.
The receiver records a `RejectedForkedStream` outcome and does not advance
the cursor through the conflicting pair. Forks are a peer-trust issue
(SRR6.5) and surface through the failure pathway, not through silent
cursor advancement.

Two distinct events at *different* `(origin_node_id, seq)` keys that share
the same `logical_memory_id` are a logical conflict. Both events are
accepted; both heads remain visible via `LogicalConflict { head_event_ids }`;
no head is hidden. Resolution is up to the agent inspecting the conflict;
the protocol does not pick a winner.

## Sync summary surface

`ee status` and `ee doctor` include a redaction-safe sync summary block
that conforms to `ee.mesh.anti_entropy.v1`. The surface intentionally
exposes only:

- last-round timestamp
- per-peer counts of `events_accepted`, `events_duplicate`, `events_forked`
- per-origin frontier counts (number of origins tracked, not the origins
  themselves)
- bounded-backoff posture: `attempts`, `next_retry_after`, `blocked_ranges`
- list of stable `degraded` codes when a round failed

Peer identities, queries, paths, and memory bodies are never rendered. The
JSON Schema rejects any drift.

## Out of scope

- Tailscale transport binding (`bd-1o1v5` SRR6.9).
- The asupersync supervisor scheduling rounds (`bd-1ylr3` SRR6.10).
- Withdrawal/tombstone propagation (`bd-kky01` SRR6.35).
- Replay-recovery / quarantine repair commands (`bd-3a2q4` SRR6.32).
- Foreground `ee mesh sync-once` CLI (`bd-2wngl` SRR6.8).
- Rolling-upgrade compatibility and schema negotiation (`bd-97rgf`
  SRR6.27).

## Stability

Wire-protocol changes that add a new message kind, change a field name in
an existing kind, or weaken the contiguous-replay invariant require an
amendment to ADR 0041. Changes that only adjust the bounded-backoff
constants or extend the sync summary additively are documented here
without an ADR amendment.
