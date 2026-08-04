# Mesh Peer Policy

Status: proposed
Bead: bd-29ulx
ADR: docs/adr/0037-optional-mesh-memory.md
Schema: docs/schemas/ee.mesh.peer_policy.v1.json
Decision schema: docs/schemas/ee.mesh.policy_decision.v1.json

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

The full mesh trust-lane vocabulary is:

| Lane | Meaning |
| --- | --- |
| `localHuman` | Local human-authored material. This is reserved for local records and must not be assigned to imported peer material. |
| `peerHumanViaPeer` | A peer claims a human authored the material on its node. It is still imported peer evidence locally. |
| `peerAgent` | A peer-side agent produced or validated the material. |
| `peerDerived` | Material is derived from a peer cache, index, graph, or summary. |
| `untrusted` | Material is retained only for audit, quarantine, or operator review. |

Peer policy records may assign only the peer-safe lanes:
`peerHumanViaPeer`, `peerAgent`, `peerDerived`, or `untrusted`. The config
parser and schema both reject `localHuman` in peer policy records.

Generic peer policy maps imported material to `agent_assertion` or
`agent_validated`. It cannot mint `peer_human_attested`; that class requires the
dedicated signed-active-member admission path and local elevation policy.
Imported material must never retain `human_explicit`, `cass_evidence`, or
`legacy_import`, even when the remote peer says a human authored the original
record.

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

Policy failures should surface as structured denied/quarantined/rejected
decisions with redaction-safe peer/workspace aliases. Raw remote workspace
paths, memory bodies, embeddings, and secrets do not belong in status, support
bundle, or handoff output unless a later explicit grant permits that lane.

Inbound failures use these stable policy-layer codes:

| Decision | Code |
| --- | --- |
| `deny` | `mesh_peer_policy_denied` |
| `quarantine` | `mesh_peer_policy_quarantined` |
| `reject` | `mesh_peer_policy_rejected` |

The failure fields include the action, reason, material lane, redaction posture,
trust lane, and a redaction-safe policy reference. Path-like policy identifiers
are replaced with stable `mesh_pol_*` aliases before they leave the policy
layer. The failure-surface schema rejects mismatched code/action pairs, so a
`*_denied` code must carry `deny`, a `*_quarantined` code must carry
`quarantine`, and a `*_rejected` code must carry `reject`.

The full inbound decision also has a stable redaction-safe JSON surface for
callers that need to expose allowed decisions:

- `action`, `reason`, `policyRef`, `materialLane`, `redaction`, `trustLane`,
  and `importTrustClass` describe the policy result.
- Allowed decisions must carry a matched redaction-safe `policyRef`; the
  `missing` sentinel is reserved for fail-closed non-allow decisions.
- Allowed decisions must use a shareable redaction posture: `share` for content
  that can leave the peer boundary or `redact` for payloads with protected body
  fields removed. `redaction=deny` is a fail-closed non-allow result.
- Allowed decisions must carry a concrete peer-safe `trustLane`
  (`peerHumanViaPeer`, `peerAgent`, `peerDerived`, or `untrusted`). `null` is
  reserved for missing-policy denial surfaces, and `localHuman` is local-only
  failure evidence, not an allowed peer-policy posture.
- `importTrustClass` is limited to `agent_assertion`, `agent_validated`, or
  `null` when no matching policy exists. Mesh peer decisions must not surface
  `peer_human_attested`, `human_explicit`, `cass_evidence`, or `legacy_import`
  as a generic imported-peer trust assignment. If a malformed in-memory policy
  attempts to use one of those restricted classes, the decision rejects it and
  suppresses the unsafe class to `null` in JSON. Allowed inbound decisions must
  use a concrete peer-safe trust class;
  `null` is reserved for missing-policy denial surfaces and rejected invalid
  policy material.
- `bodyFetchAllowed`, `localTruthSideEffectsAllowed`, and
  `searchOrGraphSideEffectsAllowed` make side effects explicit. Only allowed
  inbound body decisions may set `bodyFetchAllowed=true`; allowed non-body
  decisions must set it to `false`. Allowed inbound decisions set local-truth
  and search/graph side effects to `true`; non-allow inbound decisions set both
  to `false`.
- `failure` is either `null` for allowed decisions or the
  `ee.mesh.policy_failure_surface.v1` object described above.
- Non-allow inbound decisions (`deny`, `quarantine`, `reject`) must set all
  inbound side-effect booleans to `false`.

The same policy is checked before outbound sharing. A lane grant alone is not
enough for body or embedding payloads: if the redaction posture is `deny`, the
payload must not leave the node; if the posture is `redact`, only an already
redacted payload can be exported. Raw body or embedding payloads require both
`allow` on the lane and `share` on the matching redaction posture. Non-allow
outbound decisions must set `payloadExportAllowed` and `rawPayloadExportAllowed`
to `false`; they still report `redactedPayloadRequired=true` when the blocked
lane would have required a redacted payload.

Outbound failures use the same structured surface with outbound-specific codes
that later export/status callers can embed directly:

| Decision | Code |
| --- | --- |
| `deny` | `mesh_outbound_policy_denied` |
| `quarantine` | `mesh_outbound_policy_quarantined` |
| `reject` | `mesh_outbound_policy_rejected` |

The outbound decision JSON uses the same safe policy reference and exposes
`payloadExportAllowed`, `rawPayloadExportAllowed`, and
`redactedPayloadRequired`, so export callers can enforce body/embedding privacy
without reinterpreting policy internals. For every outbound allowed decision,
`redaction=share` permits raw payload export and leaves
`redactedPayloadRequired=false`, while `redaction=redact` permits only redacted
payload export and sets `redactedPayloadRequired=true`.

Mesh storage status reports both the total policy-decision event count and the
subset that produced policy failure surfaces so status callers can distinguish
ordinary allowed decisions from denied, quarantined, or rejected material.

The mesh import ledger persists the same `ee.mesh.policy_failure_surface.v1`
JSON in `policy_failure_surface_json` when an imported event is retained after
a denied or quarantined policy decision. Rejected events may be discarded before
ledger insertion; callers that do retain them must keep the same redaction-safe
surface shape rather than storing raw peer paths, memory bodies, or policy file
locations.

## Config Registry

Workspace-local peer policies are registered in `.ee/config.toml` under
`[[mesh.peer_policies]]`. The config parser is intentionally stricter than the
schema fixture surface: missing policy fields are configuration errors, the
default action must be `deny`, peer material can import only as
`agent_assertion` or `agent_validated`, `peer_human_attested` is reserved for
the dedicated signed-member admission path, `human_explicit`/`cass_evidence`/
`legacy_import` are rejected as restricted import trust classes, and
`localHuman` is rejected for peer policy trust lanes.

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
- `peer_policy_decision_inbound_allowed.json` pins the stable JSON shape for an
  allowed inbound metadata decision and its local side-effect booleans.
- `peer_policy_decision_inbound_redacted_body_allowed.json` pins the allowed
  inbound redacted body-fetch decision without ever promoting peer material to a
  local-only import trust class.
- `peer_policy_decision_inbound_shared_body_allowed.json` pins the allowed
  inbound raw body-fetch decision for trusted peer-agent material when the
  policy permits sharing without redaction, while still using a peer-safe
  importTrustClass.
- `peer_policy_decision_inbound_redacted_embedding_allowed.json` pins the
  allowed inbound redacted embedding decision with body fetch disabled and
  search/graph side effects permitted under a peer-safe importTrustClass.
- `peer_policy_decision_inbound_revision_notice_allowed.json` pins the allowed
  inbound revision-notice decision for freshness signaling without body fetch
  while still permitting local-truth and search/graph side effects.
- `peer_policy_decision_inbound_denied.json` pins a denied inbound body decision
  with a nested redaction-safe `ee.mesh.policy_failure_surface.v1` failure.
- `peer_policy_decision_inbound_quarantined.json` pins an inbound
  curation-signal quarantine decision with a nested redaction-safe
  `ee.mesh.policy_failure_surface.v1` failure.
- `peer_policy_decision_inbound_rejected.json` pins an inbound rejected metadata
  decision with a nested redaction-safe `ee.mesh.policy_failure_surface.v1`
  failure.
- `peer_policy_decision_outbound_metadata_allowed.json` pins the outbound
  metadata export decision shape.
- `peer_policy_decision_outbound_revision_notice_allowed.json` pins the
  outbound revision-notice export decision shape for freshness signaling without
  body or embedding exposure.
- `peer_policy_decision_outbound_redacted_body_allowed.json` pins the outbound
  redacted body export decision shape.
- `peer_policy_decision_outbound_redacted_embedding_allowed.json` pins the
  outbound redacted embedding export decision shape.
- `peer_policy_decision_outbound_shared_body_allowed.json` pins the explicit
  raw body export decision shape when the body lane and body redaction posture
  both allow sharing.
- `peer_policy_decision_outbound_shared_embedding_allowed.json` pins the
  explicit raw embedding export decision shape when the embedding lane and
  embedding redaction posture both allow sharing.
- `peer_policy_decision_outbound_denied.json` pins an outbound denied embedding
  decision with a nested redaction-safe `ee.mesh.policy_failure_surface.v1`
  failure.
- `peer_policy_decision_outbound_quarantined.json` pins an outbound quarantined
  curation-signal decision with a nested redaction-safe
  `ee.mesh.policy_failure_surface.v1` failure.
- `peer_policy_decision_outbound_rejected.json` pins an outbound rejected metadata
  decision with a nested redaction-safe `ee.mesh.policy_failure_surface.v1`
  failure.
- `peer_policy_failure_surface_denied.json` pins the standalone inbound body
  denial failure surface.
- `peer_policy_failure_surface_quarantined.json` pins the standalone inbound
  quarantine failure surface.
- `peer_policy_failure_surface_rejected.json` pins the standalone inbound
  rejected-import failure surface.
- `peer_policy_failure_surface_outbound_denied.json` pins an outbound
  embedding export denial when policy requires a redacted payload.
- `peer_policy_failure_surface_outbound_quarantined.json` pins an outbound
  quarantine failure surface.
- `peer_policy_failure_surface_outbound_rejected.json` pins an outbound
  rejected-export failure surface.

These fixtures are intentionally local and do not require real Tailscale.
