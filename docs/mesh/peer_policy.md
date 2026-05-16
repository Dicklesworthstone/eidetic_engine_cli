# Mesh Peer Policy

Status: proposed
Bead: bd-29ulx
ADR: docs/adr/0037-optional-mesh-memory.md
Schema: docs/schemas/ee.mesh.peer_policy.v1.json

## Purpose

Tailscale reachability identifies a transport peer, not an authorized memory
peer. `ee.mesh.peer_policy.v1` is the local, workspace-scoped rule that decides
which material a peer can contribute or receive and how that material must be
redacted before it affects local retrieval.

The default rule is deny. A peer policy must opt in to each workspace, origin
workspace, material lane, redaction posture, and body-fetch behavior. Missing
fields are configuration errors for policy documents and denied behavior for
runtime decisions.

## Trust Lanes

Peer material can use these trust lanes:

| Lane | Meaning |
| --- | --- |
| `localHuman` | Local human-authored material. This is reserved for local records and must not be assigned to imported peer material. |
| `peerHumanViaPeer` | A peer claims a human authored the material on its node. It is still imported peer evidence locally. |
| `peerAgent` | A peer-side agent produced or validated the material. |
| `peerDerived` | Material is derived from a peer cache, index, graph, or summary. |
| `untrusted` | Material is retained only for audit, quarantine, or operator review. |

Imported peer material maps to `agent_assertion` or `agent_validated` in the
local memory trust class. It must never import as `human_explicit`, even when
the remote peer says a human authored the original record.

## Lane Grants

The policy grants each material lane independently:

- `metadata`
- `body`
- `embedding`
- `graphLink`
- `revisionNotice`
- `curationSignal`

Each lane is `allow`, `quarantine`, or `deny`. Body and embedding grants are
separate from metadata because those lanes carry the highest privacy risk.
Metadata-only sharing is the conservative default for early mesh usage.

## Redaction And Body Fetch

The `redaction` block states whether metadata, preview, body, and embedding
surfaces may be shared, redacted, or denied. The `bodyFetch` block is an
additional explicit gate. A policy can allow metadata while setting
`bodyFetch.allowed = false`, which lets peers exchange indexes or revision
notices without exposing full memory bodies.

Policy failures should surface as structured denied/quarantined decisions with
redaction-safe peer/workspace aliases. Raw remote workspace paths, memory
bodies, embeddings, and secrets do not belong in status, support bundle, or
handoff output unless a later explicit grant permits that lane.

The same policy is checked before outbound sharing. A lane grant alone is not
enough for body or embedding payloads: if the redaction posture is `deny`, the
payload must not leave the node; if the posture is `redact`, only an already
redacted payload can be exported. Raw body or embedding payloads require both
`allow` on the lane and `share` on the matching redaction posture.

## Config Registry

Workspace-local peer policies are registered in `.ee/config.toml` under
`[[mesh.peer_policies]]`. The config parser is intentionally stricter than the
schema fixture surface: missing policy fields are configuration errors, the
default action must be `deny`, peer material can import only as
`agent_assertion` or `agent_validated`, and `localHuman` is rejected for peer
policy imports.

```toml
[[mesh.peer_policies]]
policy_id = "pol_metadata_only_001"
workspace_id = "wsp_local_release_001"
workspace_alias = "local-release"
peer_id = "peer_builder_host_001"
peer_alias = "builder-host"
origin_workspace_ids = ["wsp_remote_release_001"]
trust_lane = "peerHumanViaPeer"
import_trust_class = "agent_assertion"
default_action = "deny"

[mesh.peer_policies.allowed_lanes]
metadata = "allow"
body = "deny"
embedding = "deny"
graph_link = "quarantine"
revision_notice = "allow"
curation_signal = "deny"

[mesh.peer_policies.redaction]
metadata = "share"
preview = "redact"
body = "deny"
embedding = "deny"

[mesh.peer_policies.body_fetch]
allowed = false
requires_consent = true
max_bytes = 0
```

## Fixtures

Initial fixtures live under `tests/fixtures/mesh/`:

- `peer_policy_metadata_only.json` allows metadata and revision notices while
  denying bodies and embeddings.
- `peer_policy_body_denied.json` proves body sharing remains denied even for a
  trusted peer-agent lane.
- `peer_policy_redacted_body_allowed.json` allows bounded body fetch only with
  consent and redacted body payloads while embeddings remain denied.

These fixtures are intentionally local and do not require real Tailscale.
