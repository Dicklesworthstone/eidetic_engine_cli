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

Three surfaces are wired: graph feature enrichment (`graph_feature_enrichment`),
daemon search (`search`) and daemon pack retrieval (`context`); the daemon ones
are described below. The single-flight posture for every surface is reported,
redaction-safe, under `ee status --json` and
`ee doctor --json` at `data.singleFlight` (and folded into `ee swarm brief`).
Those commands report their own process. The daemon's counts are in its
`ee.daemon.telemetry` response.

## Daemon search (opt-in)

`ee daemon` serves each connection on its own worker, so agents running
`ee search --use-daemon`, `ee context --use-daemon` or `ee pack --use-daemon`
at the same moment reach the same process. With `EE_DAEMON_SEARCH_SINGLEFLIGHT=on` in the
daemon's environment, identical concurrent `ee.daemon.search` requests run one
search, and identical concurrent `ee.daemon.pack_search` requests run one pack
retrieval (bd-d67os.9). It is off by default.

- **The key** holds the exact query and every search option, the workspace
  store's generation, the user-global store's generation when the read may use
  it, the published index generation and the merged config. It also binds each
  store's complete feedback ledger, pending review-hold metadata, and the
  calibration bytes used by retrieval. Inserts, same-count edits, application
  markers, quarantine retargeting or review, and same-size calibration-file
  replacements distinguish requests even when no generation advances. These
  witnesses use the same identity rules as pack handoffs and short pinned
  database snapshots, released before waiting for a leader. A search key adds
  the kind and typed field filters. A pack retrieval key adds whether a cached
  local model is required and which model is loaded. Missing or unreadable
  authority disables coalescing for that request; canonical retrieval still runs.
- **Audit.** The shared search runs without its retrieval audit, and every
  request then records its own. N coalesced requests leave N
  `search.executed` rows, as N uncoalesced ones do. Pack retrieval writes
  nothing in the daemon; each client records its own pack.
- **Not coalesced:** `ee.daemon.context`, which assembles and stores a whole
  pack, and the swarm brief. `ee serve` answers one connection at a time and
  runs the brief as a command, so no process ever computes two briefs at
  once.

A default-time follower evaluates validity windows at the leader's search
instant, slightly before its own arrival. This difference remains bounded by
the leader's run time. Explicit `as_of` requests carry their reference time in
the key. Pack clients also revalidate source and calibration witnesses before
using a handoff, so a change after the daemon captured its key can still force
fresh local retrieval. Pending quarantine holds participate in that identity:
a new hold cannot reuse an earlier candidate pool, and removing a hold triggers
fresh selection instead of retaining a pool that omitted the held memory.
Only hold IDs, owning workspace, target types and target IDs enter the hash;
review reasons, source identities and quarantined payloads are never read for it.

## Agent UX

What an agent can rely on today:

- **In-process bursts coalesce.** When one `ee` process issues many identical
  concurrent reads (e.g. a fan-out inside a single command), they collapse to
  one computation and all callers get a byte-identical result.
- **Separate commands are independent.** Running `ee search '<q>'` in two shells
  does not coalesce across the two processes. If you want the savings, batch the
  work inside one invocation, or send both searches through a daemon running
  with `EE_DAEMON_SEARCH_SINGLEFLIGHT=on` (`ee search --use-daemon`).
- **Changed inputs run independently.** Source/index generation changes and
  complete feedback/calibration and pending-review witnesses distinguish
  post-write requests from earlier in-flight work. The key carries hashes,
  never raw feedback reasons, quarantine payloads or calibration rows.
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
