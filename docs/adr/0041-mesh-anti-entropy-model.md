# ADR 0041: Mesh Anti-Entropy Model

Status: proposed
Date: 2026-05-19
Bead: bd-3lx0p (SRR6.7) + bd-33zh3 (SRR6.25)

## Context

ADR 0037 establishes optional mesh memory as a local-first cache plus explicit
revision channel. It is not a distributed database. The remaining problem is
how two `ee` peers reconcile their event streams after partitions, restarts,
duplicate deliveries, and out-of-order arrival without coordination — i.e.
without distributed consensus.

`ee` does not need linearizability for memory facts. Memories, links,
embeddings, and revisions are append-only evidence. The product contract
(ADR 0007) is that context packs and search results are returned from
deterministic local state, with later peer evidence surfacing as revision
notifications, not as silent rewrites. The anti-entropy layer is what makes
that contract honest under realistic network failures.

SRR6.25 (`bd-33zh3`) introduced a pure executable model
(`src/mesh/anti_entropy_model.rs`) that captures the local invariants this
ADR commits to. SRR6.7 (`bd-3lx0p`) owns the protocol surface and the
repository support that implements those invariants for real cursors,
ranges, and sync summaries. This ADR is the joint design contract; the
runtime work is split across multiple sibling beads so it can land in narrow
slices.

## Decision

Mesh anti-entropy is **per-origin append-only stream reconciliation with
durable contiguous-replay cursors**. It is not gossip, not Paxos, not CRDT
merging. It is the smallest mechanic that lets two peers converge after a
partition while preserving each event's origin attribution.

The model has five pieces:

1. **Event keys are `(origin_node_id, seq)` and immutable.** Every mesh
   event carries the origin node ID that produced it and a monotonically
   increasing per-origin sequence number. The pair is the canonical
   ordering. No global sequence number exists.
2. **Frontiers/cursors are derived from contiguous replay only.** A node's
   cursor for `origin_node_id=X` advances from `n` to `n+k` only after every
   event in `X[n+1..=n+k]` has been durably accepted. Holes block cursor
   advancement until the missing range arrives, even if later events for the
   same origin have already been written to the local accepted set.
3. **Anti-entropy exchange is `(my_frontier, peer_frontier) → ranges_to_request`.**
   When two peers sync, each side computes the contiguous-cursor ranges for
   which the peer has events the local side has not durably replayed. Ranges
   are requested one origin at a time, starting at `local_cursor + 1`, so
   gaps cannot be silently skipped.
4. **Replay is idempotent and rejects forks.** Re-delivery of an event with
   the same `(origin_node_id, seq, event_id)` is `Duplicate` (no-op).
   Delivery of a *different* `event_id` for the same `(origin_node_id, seq)`
   is a `RejectedForkedStream` outcome. The replayer never overwrites an
   existing accepted event.
5. **Logical conflicts are evidence, not resolved.** Two events can validly
   carry the same `logical_memory_id` with different `base_event_id`
   parentage. The model surfaces this as a `LogicalConflict { head_event_ids }`
   for the agent to inspect; the replayer does not pick a winner.

Read semantics are local-cache / bounded-staleness, not linearizability:

- Tier 1 reads always return the current local accepted state immediately.
- Later replay that advances a frontier emits a `RevisionNotice` with the
  set of `(origin_node_id, from_seq, to_seq)` advances since the previous
  read snapshot.
- Agents that need fresher reads call the explicit refresh path; they do not
  retry until linearizable agreement.

## Wire protocol shape (sibling-bead detail)

The wire protocol carrying frontier advertisements, range requests, event
batches, and bounded retry/backoff lives in
`docs/mesh/anti_entropy.md`. This ADR commits only to the four message
kinds, the deterministic naming, and the invariant that no message kind
mutates remote state without passing through the same policy gates as
local imports.

Sync summaries (the surface `ee status` and `ee doctor` consume) are pinned
by `docs/schemas/ee.mesh.anti_entropy.v1.json`. The schema is part of this
ADR contract — drift requires an ADR amendment.

## Invariants

- A cursor for an origin advances **only** by `+1` per contiguous accepted
  event; no skip-ahead is allowed even with future evidence in hand.
- The replayer is idempotent: `replay(e)` followed by `replay(e)` produces
  the same accepted set and the same frontier as a single `replay(e)`.
- The replayer is order-independent over *contiguous* event sets: delivering
  `X[1..=n]` in any order that respects contiguity yields the same final
  accepted set and frontier.
- The replayer **never** mutates an existing accepted event; a forked
  `(origin, seq)` is reported, not merged.
- Logical conflicts are visible at the `logical_memory_id` level; the model
  never hides one head behind another.
- Partition + rejoin with duplicate or out-of-order delivery converges to
  the same accepted set and frontier as a synchronous in-order replay.
- Range requests are bounded: a single request covers one origin and one
  contiguous span; a single anti-entropy round emits at most one range per
  origin.
- Bounded retry/backoff is required for every range request (sibling bead
  `bd-9ygik.6` family + this ADR's invariant). Unbounded retry violates
  ADR 0037's "no daemon-required core workflow" budget.

## Threat model

Anti-entropy reuses the SRR6 threat model from ADR 0037. New threats
specific to convergence:

| Threat | Required control |
| --- | --- |
| Forged origin/seq pair | Event ID includes `(origin, seq, logical_id, content_hash)` and the replayer rejects mismatched re-deliveries as `RejectedForkedStream` |
| Cursor poisoning via skip-ahead | Contiguous-replay-only invariant: cursor moves only when every prior seq is accepted |
| Duplicate-delivery amplification | Idempotent `replay`; the `Duplicate` outcome is a no-op at every layer |
| Partition rejoin starvation | Bounded retry/backoff with explicit retry-after evidence; sibling bead `bd-1ylr3` owns the supervisor budget |
| Stale Tier 1 reads silently hiding peer evidence | Explicit `RevisionNotice` surface; never a silent rewrite of a returned pack |
| Range-request amplification DoS | Single anti-entropy round emits at most one bounded range per origin |
| Logical-conflict hiding | `LogicalConflict { head_event_ids }` surfaced explicitly — no automatic winner picking |
| Cross-workspace leakage via sync summary | `ee.mesh.anti_entropy.v1` schema redacts peer identities, paths, and queries; status/doctor render only counts and stable enums |

## Scenarios (executable test contract)

The constants in `src/mesh/anti_entropy_model.rs::ANTI_ENTROPY_MODEL_SCENARIOS`
name the five reference scenarios this ADR commits to:

1. `cursor_advances_only_after_contiguous_replay`
2. `partition_rejoin_duplicate_out_of_order_delivery`
3. `conflicting_revisions_are_visible`
4. `stale_tier1_read_gets_revision_notice`
5. `deterministic_replay_order_independent`

Adding a scenario requires both an ADR amendment and a corresponding entry
in that constant array, so the test list and the contract cannot drift.

The focused executable harness is `tests/mesh_anti_entropy_model.rs`. It
imports the model directly while the runtime `src/mesh/mod.rs` surface is in
flight, replays the scenario tests, and prints
`mesh_anti_entropy_model_scenario=<name> result=covered` lines in `--nocapture`
mode. The e2e driver `scripts/e2e_mesh_anti_entropy_model.sh` emits matching
`ee.test_event.v1` JSONL records before running the focused harness so replay
logs can be correlated with the formal scenario catalog.

## Out of scope for this ADR

- The transport (Tailscale or otherwise) — owned by `bd-1o1v5` (SRR6.9).
- The asupersync supervisor that schedules anti-entropy rounds — owned by
  `bd-1ylr3` (SRR6.10).
- Withdrawal / tombstone propagation semantics — owned by `bd-kky01`
  (SRR6.35).
- The replay-recovery and quarantine repair commands — owned by `bd-3a2q4`
  (SRR6.32).
- Foreground `ee mesh` CLI surfaces — owned by `bd-2wngl` (SRR6.8).

## Consequences

- A single pure model in `src/mesh/anti_entropy_model.rs` is the executable
  source of truth for the invariants above; every later mesh-protocol slice
  must keep it green.
- `ee status` and `ee doctor` gain a stable `ee.mesh.anti_entropy.v1`
  surface for sync summaries (counts, last round, bounded backoff posture)
  without exposing peer identities, paths, or query text.
- Cursor mutation is allowed only by the durable replay path; no
  CLI/MCP/import surface may advance a cursor directly. The DB-level
  enforcement lives in the repository support landing under sibling beads.
- Bounded retry/backoff is a hard invariant. Any future supervisor work that
  needs unbounded retry is a separate ADR amendment.
- The wire protocol may evolve under `docs/mesh/anti_entropy.md` without an
  ADR amendment so long as the four message kinds, the contiguous-replay
  invariant, and the sync-summary schema remain unchanged.
