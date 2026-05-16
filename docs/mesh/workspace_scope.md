# Mesh Workspace Scope And Namespace Isolation

Status: proposed
Bead: bd-2jb3s
ADR: docs/adr/0037-optional-mesh-memory.md

## Purpose

Optional mesh memory must not turn peer trust for one repository into implicit
trust for every repository on the same machine. A peer can be useful for one
workspace and forbidden for another. This document defines the workspace and
peer-group boundary that later SRR6 event, policy, storage, replay, and query
surfaces must implement.

The core rule is default deny:

```text
local workspace + peer group binding + local policy grant
  => mesh material may be imported, cached, displayed, or queried

missing binding, unknown workspace, or denied lane
  => quarantine or reject; never merge into local truth or query results
```

## Terms

- Local workspace: the workspace resolved from the current `--workspace` path.
  It is identified by the same stable workspace id used by local memory storage.
- Origin workspace: the workspace id asserted by a mesh event producer. It is
  data inside the event envelope, not authority by itself.
- Peer: a stable node identity known to the local machine. Tailscale reachability
  is transport evidence only; it is not authorization.
- Peer group: a local configuration object that binds one or more peers to one
  local workspace and a set of allowed lanes.
- Lane: a separately grantable material class such as metadata, body,
  embedding, graph link, revision notice, or curation signal.
- Quarantine: durable local holding state for a received event that is
  well-formed enough to audit but not authorized for import into the workspace.

## Workspace Identity

Every mesh event must carry:

- `originWorkspaceId`
- `originWorkspaceLabel` or redaction-safe display alias
- `producerPeerId`
- `eventId`
- `eventSchema`
- `materialLane`
- policy/redaction posture for the payload

The receiver must compare `originWorkspaceId` to the peer-group bindings for the
current local workspace. A matching string is necessary but not sufficient:
the local config must bind `producerPeerId` to that origin workspace and grant
the event lane.

Local workspace paths are never used as wire identity. Paths can leak private
project names and differ across machines. User-facing displays may show the
local workspace label, but mesh status and support bundles should default to a
short redaction-safe namespace alias.

## Peer Group Binding

Peer groups are scoped to one local workspace. Reusing a group profile across
workspaces is allowed only by explicit opt-in duplication: each workspace must
record its own binding to the group id and each lane grant must be visible in
that workspace's config or policy surface.

Minimum binding fields for later config/schema work:

```json
{
  "schema": "ee.mesh.peer_group_binding.v1",
  "workspaceId": "wsp_...",
  "peerGroupId": "pg_...",
  "peerIds": ["peer_..."],
  "originWorkspaceIds": ["wsp_remote_..."],
  "lanes": {
    "metadata": "allow",
    "body": "deny",
    "embedding": "deny",
    "graphLink": "allow",
    "revisionNotice": "allow",
    "curationSignal": "quarantine"
  },
  "defaultAction": "deny"
}
```

Unknown peers, unknown origin workspaces, missing lane entries, and malformed
bindings all resolve to deny. A denial may be recorded as an audit decision, but
it must not create local memories, tags, graph edges, embeddings, body cache
rows, search documents, or curate candidates.

## Import Decisions

Mesh import must produce one explicit decision per event:

| Decision | Meaning | Allowed side effects |
| --- | --- | --- |
| `allow` | Binding and lane grant permit the material | Write import ledger, cache row, derived index artifact, or explicit local revision according to the owning bead |
| `quarantine` | Event is parseable but not importable yet | Write quarantine/audit state only |
| `deny` | Event is not authorized or violates policy | Write audit decision only |
| `reject` | Event is malformed or unsafe to retain | Write audit decision only, with payload discarded |

The decision record must include `workspace_scope_decision`, `workspace_id`,
`origin_workspace_id`, `peer_group_id`, `producer_peer_id`, `material_lane`,
`allowed`, and `reason`. These fields are required for structured logs and for
the future e2e isolation script.

## Query And Display Rules

`ee search`, `ee context`, and `ee why` may surface cached mesh material only
after it has passed the import decision for the current local workspace. They
must preserve namespace provenance in machine output:

- local memory id or cached remote material id
- origin workspace alias, not raw remote path by default
- producer peer id or redacted peer label
- material lane
- import decision id or ledger cursor
- trust lane and redaction posture

Human output should avoid leaking other workspace names in status summaries.
`ee mesh status` may report counts by namespace alias, lane, and posture, but
it must not list raw workspace paths, queries, memory bodies, or tags for
unauthorized workspaces.

## Cross-Workspace Isolation Requirements

The following must be impossible without an explicit binding in the current
workspace:

- importing a remote memory body
- indexing remote metadata or body text for search
- adding remote tags to local tag statistics
- adding remote graph links to local graph projections
- using remote embeddings or search surrogates
- surfacing remote evidence in `ee why`
- allowing remote outcome or curation signals to change local confidence
- emitting another workspace's name through status, doctor, support bundle, or
  handoff surfaces

Denied material can still be counted in redacted operational telemetry when the
caller has enabled mesh, because operators need to know that something was
blocked. The count must be by reason and lane, not by secret-bearing payload.

## Verification Obligations

Later implementation beads must add executable proof for this contract:

- Unit: same peer trusted in workspace A and unbound in workspace B cannot import
  the same event into B.
- Unit: lane-specific grants allow metadata while denying body and embedding.
- Unit: unknown origin workspace denies by default.
- Unit: quarantine preserves enough redacted evidence for audit without indexing
  payload content.
- E2E: two local workspaces and one shared peer; importing material into A does
  not affect `search`, `context`, `why`, graph projection, or status in B.
- E2E: `ee mesh status --json` reports denied/quarantined counts without raw
  remote workspace names or memory bodies.
- Log contract: every import path emits the `workspace_scope_decision` fields
  listed above.

Passing those tests is the closure signal for bd-2jb3s. This document is only
the contract the tests and implementation must satisfy.
