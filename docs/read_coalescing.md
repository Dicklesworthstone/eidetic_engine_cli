# Read coalescing (single-flight)

Read coalescing folds concurrent, *identical* read-heavy computations into a
single execution: the first caller becomes the **leader** and runs the work
once; every other caller for the same key becomes a **follower** that waits for
the leader and reuses its result. This cuts redundant CPU and database pressure
when a swarm of agents asks the same expensive question at the same time.

It is implemented by `crate::core::singleflight` (Track A of the swarm-scale
hot-path epic, bd-d67os) and is **process-local** — see
[Scope: process-local](#scope-process-local) and
[Cross-process coalescing (future, not implemented)](#cross-process-coalescing-future-not-implemented).

## How it works

- **Key.** Each coalescable request derives a `SingleFlightKey` from its
  normalized inputs *plus the workspace and graph generation*. Because the
  generation is part of the key, a write that bumps the workspace/graph
  generation produces a different key, so a stale in-flight computation can
  never be reused across a write — the new request runs its own leader. This is
  how a write "busts" coalescing without any explicit invalidation.
- **Leader / follower.** The first caller to register a key is the leader and
  runs the operation. Concurrent callers for the same key join as followers and
  block on a condition variable until the leader completes, then clone the
  leader's `Ok` value (or observe the same `Err`).
- **Follower timeout (fail-open).** A follower that waits longer than the
  surface's `follower_timeout` stops waiting and falls back to computing the
  value itself; it never cancels the leader. A timeout is surfaced as the
  `singleflight_follower_timeout` degraded code, not a hard error.
- **Leader failure / panic.** If the leader returns `Err` or panics, the entry
  is cleared and followers observe the failure (`singleflight_leader_failed`)
  rather than hanging; a poisoned mutex is counted and never silently swallowed.

## Scope: process-local

A single-flight group lives in process memory (`OnceLock<SingleFlightGroup<…>>`).
Coalescing therefore happens **only between concurrent requests inside the same
`ee` process**. Two separate `ee` CLI invocations — even identical ones started
at the same instant — do **not** share a group and each runs its own
computation. This is intentional for v1: it needs no shared state, no liveness
protocol, and cannot serve a stale cross-process result.

The currently wired surface is graph feature enrichment
(`graph_feature_enrichment`). The single-flight posture for every configured
surface is reported, redaction-safe, under `ee status --json` and
`ee doctor --json` at `data.singleFlight` (and folded into `ee swarm brief`).

## Agent UX

What an agent can rely on today:

- **In-process bursts coalesce.** When one `ee` process issues many identical
  concurrent reads (e.g. a fan-out inside a single command), they collapse to
  one computation and all callers get a byte-identical result.
- **Separate commands are independent.** Running `ee search '<q>'` in two shells
  does not coalesce across the two processes. If you want the savings, batch the
  work inside one invocation rather than spawning N identical processes.
- **Writes are safe.** A coalesced read can never return data from before a
  write you just made in the same workspace: the generation in the key changes,
  so post-write reads re-run.
- **Observe it.** The hidden e2e harness exercises the live coalescer with the
  real binary:

  ```bash
  ee graph feature-enrichment --dry-run \
      --singleflight-burst 8 --singleflight-distinct 3 --json
  ```

  `data.summary.identicalLeaderCount` is `1` (one computation),
  `data.summary.identicalFollowerCount` is `N-1` (all coalesced),
  `data.resultHashes.identicalUniqueCount` is `1` (one shared result), and the
  `D` deliberately-distinct keys each get their own leader
  (`distinctLeaderCount == D`). `scripts/e2e_read_coalescing.sh` asserts this
  end to end with `ee.test_event.v1` structured logs.

## Cross-process coalescing (future, not implemented)

A future option could extend coalescing across processes so that a swarm of
separate `ee` invocations sharing a workspace also pays the cost once. **This is
a design sketch only; it is not implemented.**

**Sketch.** Keyed on the same generation-bound `SingleFlightKey`:

1. A process that misses the L2 pack/search cache attempts to claim an advisory
   *leader sentinel* in shared workspace state (a short-TTL row/file keyed by the
   single-flight key hash). Claiming is best-effort and advisory — it never
   gates correctness or claim authority.
2. The sentinel winner is the cross-process leader: it computes the value,
   populates the shared L2 cache, then clears the sentinel.
3. Sentinel losers (followers) wait-then-read: they poll the L2 cache up to a
   bounded timeout, and on a hit reuse the leader's result. On timeout they fall
   back to computing locally — identical fail-open semantics to the in-process
   follower timeout.

**Risks that must be designed for before implementing.**

- **Staleness.** The shared result must remain generation-bound; a follower must
  re-check the workspace/graph generation between sentinel-wait and L2-read so a
  concurrent write cannot hand back a pre-write value. Cross-process windows are
  wider than in-process ones, so the staleness surface is larger.
- **Leader death.** A leader that crashes or is killed mid-computation must not
  wedge followers. The sentinel needs a TTL and a liveness signal (e.g. an
  owner pid/heartbeat) so followers fail open promptly instead of waiting the
  full timeout on every request.
- **Fairness / thundering herd.** Sentinel contention must not starve any caller
  or convert into a thundering herd on timeout; back-off and a capped follower
  wait are required, and the path must stay strictly advisory so a degraded
  sentinel store can never block real work.
- **Sentinel GC and store coupling.** Orphan sentinels must be reaped, and the
  feature must degrade cleanly (to today's process-local behavior) whenever the
  shared store is unavailable, read-only, or under disk pressure.

Until those are resolved, read coalescing remains process-local by design.

## Telemetry reference

`data.singleFlight` (schema `ee.singleflight.posture.v1`) per surface:

| field | meaning |
| --- | --- |
| `activeLeaderCount` | leaders currently computing |
| `leaderStartCount` | leader computations started |
| `followerJoinCount` / `followerWaitCount` | callers that coalesced behind a leader |
| `followerTimeoutCount` | followers that fell back after `followerTimeoutMs` |
| `leaderFailureCount` | leader errors/panics observed by followers |
| `reusedResultCount` | coalesced results handed to followers |
| `statePoisonedCount` | poisoned-mutex fallbacks (never silently swallowed) |
