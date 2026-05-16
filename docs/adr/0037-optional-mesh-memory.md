# ADR 0037: Optional Mesh Memory

Status: proposed
Date: 2026-05-15
Bead: bd-1653f

## Context

`ee` is a local-first, CLI-first memory substrate for coding agents. ADR 0001
keeps the agent harness in charge. ADR 0002 makes FrankenSQLite and SQLModel
the local source of truth. ADR 0007 makes context packs the primary product
surface, and ADR 0017 describes swarm-scale operation without turning `ee` into
a scheduler.

SRR6 proposes optional Tailscale-based mesh memory for teams or swarms that use
several machines. That goal is useful, but it is easy to over-read it as a
distributed database, a required daemon, or a direct peer-to-peer replacement
for the local store. Those interpretations would violate the project contract.

The current implementation does not have canonical JSONL memory logs that can
be gossiped as-is, and it does not have a ready network daemon socket for memory
traffic. The durable store is local FrankenSQLite/SQLModel, with derived indexes,
graph snapshots, cache records, audit rows, and optional foreground daemon jobs.
SRR6 must therefore introduce explicit mesh event and replay surfaces instead
of pretending the existing database can be replicated blindly.

## Decision

SRR6 is an optional local-first mesh cache plus explicit revision channel.
It is not a distributed database.

With mesh disabled, `ee` starts no listener, opens no network connection, reads
no peer configuration, and emits byte-stable results for ordinary local commands.
Core workflows such as `ee remember`, `ee search`, `ee context`, `ee pack`,
`ee why`, and `ee status` continue to work without a daemon, network, Tailscale,
or peer identity.

With mesh enabled, `ee` uses a two-tier model:

1. Tier 1 returns deterministic local results from the local DB, local indexes,
   local graph snapshots, and any peer material already imported into the local
   cache with provenance.
2. Tier 2 performs bounded asynchronous freshness work. Peer probes may discover
   newer remote material, but they do not silently rewrite the already returned
   context pack. They emit explicit revision tokens, revision notifications, or
   follow-up cache-import events.

All remote material is treated as imported evidence. It carries origin,
producer, peer identity, trust lane, policy decisions, redaction posture, and
import ledger provenance. A peer cannot write directly into the local truth
store without passing through append-only event import, policy checks, and audit
recording. Remote failures degrade only mesh-enabled surfaces.

Tailscale is a transport and discovery convenience, not a trust boundary.
Being reachable over Tailscale does not make a peer authorized to share bodies,
embeddings, graph links, or revision notifications. Authorization, redaction,
peer scope, and trust class are owned by `ee` policy and local configuration.

## Invariants

- Mesh is disabled by default.
- Disabled mesh has zero network activity and zero daemon requirement.
- Local command output remains deterministic when mesh is disabled.
- Mesh-enabled local results are still returned from local state first.
- Peer probes cannot mutate a returned pack or search result silently.
- Remote memories, links, embeddings, and revisions are imported evidence, not
  local truth.
- Every remote artifact carries origin, provenance, producer, peer identity,
  origin workspace, trust lane, and policy/redaction metadata.
- Body and embedding sharing is policy-gated independently from metadata
  sharing.
- Remote failures affect only mesh-enabled surfaces and must appear as typed
  degraded entries with repair guidance.
- Mesh mutations are append-only mesh events, local cache refreshes, import
  ledger writes, or explicit local revisions.
- No SRR6 slice may introduce Tokio, Hyper, Axum, Tower, Reqwest, or a
  daemon-required core workflow.

## Threat Model

SRR6 must assume that reachable peers are not automatically trustworthy. The
main risks are:

| Threat | Required control |
| --- | --- |
| Accidental network activation | Mesh-off no-network regression gate and config defaults pinned to disabled |
| Peer impersonation | Stable node identity, explicit peer allowlist, and signed or authenticated event envelopes |
| Unauthorized body or embedding sharing | Peer policy that separates metadata, body, embedding, and graph-link permissions |
| Poisoned remote memory | Trust lanes, provenance, import ledger, and local quarantine or low-confidence defaults |
| Cross-workspace leakage | Default-deny workspace scope, explicit peer-group binding, and namespace-isolation tests |
| Replay or forked history | Mesh event IDs, source cursors, import ledger idempotency, and revision notifications |
| Silent context drift | Revision tokens instead of mutating already emitted packs or search results |
| Topology leakage | Redaction-safe peer status that avoids exposing memory bodies, queries, or secrets |
| Partition/rejoin inconsistency | Anti-entropy cursor protocol plus deterministic replay and convergence tests |
| Transport compromise | Treat Tailscale as transport only; require `ee` authorization and policy checks above it |

The threat model is local-first. A user who never enables mesh should not pay
latency, privacy, conceptual, or operational cost for these controls.

## Reserved Public Schemas

The first SRR6 implementation slices should reserve and document these schema
names before emitting them:

| Schema | Purpose |
| --- | --- |
| `ee.mesh.event.v1` | Append-only mesh export/import event envelope |
| `ee.mesh.peer_policy.v1` | Local peer authorization, trust, and redaction policy |
| `ee.mesh.revision_notice.v1` | Explicit notice that fresher peer material is available for a returned result |
| `ee.mesh.peer_status.v1` | Redaction-safe peer/cache/status posture |
| `ee.mesh.import_ledger.v1` | Local replay cursor and idempotency record for peer events |

These names reserve contract intent, not implementation permission. Each schema
still needs a drift test, fixture, and owning bead before it becomes public API.

## Implementation Beads And Proof Obligations

The SRR6 bead graph should keep this order and scope. Later agents should treat
the named beads as part of the intended design, not as optional embellishments:

1. `bd-1653f` records this ADR and threat model.
2. `bd-x4hn7` proves mesh-off optionality, no-network behavior, config
   defaults, and capability posture.
3. `bd-2gtjn` defines event schema, node identity, and global memory identity.
4. `bd-2jb3s` defines workspace scope, peer groups, and namespace isolation.
5. `bd-2cndm` adds peer, cursor, and import-ledger storage.
6. `bd-29ulx` implements peer trust, authorization, and redaction policy.
7. `bd-1msdr` proves deterministic local export/import replay.
8. `bd-3lx0p` adds anti-entropy tips, ranges, and convergence.
9. `bd-2wngl` exposes foreground `ee mesh` CLI commands.
10. `bd-1o1v5` adds the optional Tailscale transport adapter without forbidden
   network crates.
11. `bd-1ylr3` adds an optional Asupersync background mesh supervisor with
    budgets and cancellation; it must not make daemon mode mandatory.
12. `bd-273tl`, `bd-wl4ja`, and `bd-1n438` define eager metadata caching,
    policy-gated lazy body fetch, local-first cached-peer search, and bounded
    asynchronous freshness probes.
13. `bd-37ptl` adds explicit pack/search revision tokens for two-tier results.
14. `bd-1lgq6` projects cached remote memories and links into local graph
    features without turning remote graph state into local truth.
15. `bd-162sk`, `bd-3k16v`, `bd-3i5q7`, `bd-3url9`, and `bd-ghey6` provide
    mesh-off, replay convergence, privacy/redaction, latency/resource, and
    local two-node fixture proof.
16. `bd-nw0v3` and `bd-30ilt` add observability, doctor/status surfaces,
    failure-mode catalog entries, operator docs, and agent onboarding.
17. `bd-2vu8m` performs final integration hardening and closeout verification.
18. `bd-1x87h` adds peer enrollment, capability handshake, and key rotation UX.
19. `bd-33zh3` formalizes anti-entropy, stale-read, and revision semantics.
20. `bd-168gm` defines mesh cache retention, quotas, eviction, and body
    lifecycle behavior.
21. `bd-97rgf` covers rolling-upgrade compatibility and schema negotiation.
22. `bd-1fjhu` allows learn/curate flows to use cached remote evidence without
    promoting untrusted remote claims directly into local procedural truth.
23. `bd-26d7w` keeps the verification matrix and structured e2e logging
    contract aligned with the whole SRR6 surface.

The minimum proof set is:

- Mesh-off byte-stability and no-network regression tests.
- Forbidden-dependency audit showing no prohibited network stack entered core.
- Schema drift tests for every `ee.mesh.*.v1` contract that is emitted.
- Deterministic replay tests for exported mesh events.
- Policy tests for metadata-only, body-sharing, embedding-sharing, and denied
  peer lanes.
- Partition/rejoin tests that prove idempotent convergence without silent local
  truth mutation.
- E2E tests using a local two-node fixture without real Tailscale.
- Enrollment and key-rotation tests showing that transport reachability is
  insufficient without local peer authorization.
- Workspace-scope tests showing a peer trusted in one workspace cannot receive,
  import, or influence memories, tags, graph edges, embeddings, or bodies from
  another workspace without an explicit peer-group binding.
- Retention, quota, eviction, and body-lifecycle tests proving cache cleanup
  cannot erase local source-of-truth memories.
- Rolling-upgrade and schema-negotiation tests for mixed-version peers.
- Learn/curate tests proving remote evidence remains provenance-tagged and
  policy-gated before it can affect local rule confidence.
- Formal-model checks or executable invariants for stale reads, revision
  notices, anti-entropy replay, and convergence.

## Consequences

SRR6 can improve multi-machine swarms without making single-machine users carry
distributed-systems cost. Agents get a path to fresh remote evidence while
retaining deterministic local context packs and explicit provenance.

The design is slower to implement than direct replication. That is intentional:
remote material must cross policy, provenance, and replay boundaries before it
can influence local retrieval.

Tailscale-specific code remains an adapter. The core mesh contracts must be
testable in a local fixture so CI and ordinary contributors do not need a real
tailnet.

## Rejected Alternatives

- **Eager full replication**: Rejected because it turns local memory into a
  distributed database, increases privacy blast radius, and makes mesh-off users
  pay for a feature they did not enable.
- **Purely federated search**: Rejected because ordinary `ee context` would
  become network-latency-bound and non-deterministic whenever peers are slow or
  partitioned.
- **CRDT-first global graph**: Rejected because graph snapshots are derived
  assets and because global graph mutation would obscure provenance and local
  policy decisions.
- **HTTP/gRPC transport**: Rejected because Hyper, Axum, Tower, Reqwest, and
  common gRPC stacks violate the forbidden-dependency boundary and add protocol
  surface before local contracts are proven.
- **Daemon-required operation**: Rejected because CLI-first operation is a core
  project invariant. A daemon may accelerate mesh sync later, but no core local
  command may require it.
- **Tailscale as trust**: Rejected because network membership is not equivalent
  to authorization for memory bodies, embeddings, graph links, or curation
  signals.

## Verification

This ADR is actionable only when later SRR6 slices attach executable evidence:

- `bd-x4hn7` must add mesh-off tests that prove no network activity, no listener,
  no peer config requirement, and byte-stable local command output.
- `bd-2gtjn`, `bd-2jb3s`, and `bd-29ulx` must add schema fixtures, namespace
  isolation tests, and policy tests for mesh event, workspace scope, and peer
  policy contracts.
- `bd-1msdr` and `bd-3k16v` must prove deterministic replay and partition/rejoin
  convergence with local fixtures.
- `bd-3i5q7` must prove privacy, redaction, and authorization behavior in e2e.
- `bd-ghey6` must provide a no-real-Tailscale two-node demo fixture.
- `bd-1x87h` must prove enrollment, capability handshake, and key rotation do
  not treat Tailscale reachability as authorization.
- `bd-168gm` must prove retention, quotas, eviction, and body lifecycle preserve
  local truth and respect policy.
- `bd-97rgf` must prove rolling-upgrade compatibility and mesh schema
  negotiation across mixed peers.
- `bd-1fjhu` must prove peer-aware learn/curate behavior is evidence-based,
  provenance-tagged, and policy-gated.
- `bd-33zh3` must provide the formal model or executable invariant suite for
  anti-entropy, stale reads, and revision semantics.
- `bd-2vu8m` must audit that every emitted mesh response degrades cleanly and
  that mesh-disabled users still see no behavior change.

Passing these tests is stronger evidence than this document. Until they exist,
SRR6 remains a proposed design with explicit invariants, not an implemented
distributed memory layer.
