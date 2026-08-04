# Optional Mesh Operator Onboarding

Status: proposed
Bead: bd-30ilt
ADR: docs/adr/0037-optional-mesh-memory.md

## Purpose

Optional mesh memory lets a local `ee` installation use policy-approved
material from other nodes without making ordinary `ee` commands depend on a
network, daemon, or tailnet. This guide is for operators and coding agents who
need to decide whether mesh is appropriate for a workflow, how to enable it
safely, and how to explain degraded mesh results without overstating what was
verified.

For agent-specific decision rules, pair this guide with
`docs/agent-ux/auto_enrollment_onboarding.md`. Agents should treat that
onboarding note as the operational checklist for staying local-only, inspecting
mesh read-only, dry-running enrollment, and handling revisable packs.

The default remains local-first:

- `ee init`, `ee remember`, `ee search`, `ee pack`, `ee why`, and
  `ee status` do not require mesh.
- Mesh disabled means no listener, no peer probing, and no peer configuration
  requirement for core workflows.
- Tailscale reachability is only transport reachability. Authorization,
  redaction, trust class, body fetch, embedding export, and workspace scope are
  still decided by local `ee` policy.

## When To Use Mesh

Use mesh when the operator deliberately wants one of these behaviors:

| Scenario | Good fit | Why |
| --- | --- | --- |
| One developer, one machine | No | Local `ee` already has the source of truth and derived indexes. |
| Several machines owned by the same operator | Sometimes | Mesh can share metadata, revision notices, or redacted evidence across trusted nodes. |
| Large agent swarm split across hosts | Yes, with policy | Mesh can reduce repeated rediscovery, but every lane still needs explicit grants. |
| A contractor or untrusted peer | No by default | Reachability is not authorization, and peer material imports as peer evidence. |
| Emergency incident response | Only after review | Start with metadata-only and audit logs; avoid body or embedding lanes until approved. |

Do not enable mesh just to make `ee pack` faster on one host. Use local
indexes, graph snapshots, pack caches, and RCH-friendly verification first.

## Core Model

Mesh uses two tiers:

| Tier | Source | User-visible contract |
| --- | --- | --- |
| Tier 1 | Local database, local indexes, local graph snapshots, and already imported authorized cache rows | Returns the immediate answer. It remains deterministic for the same local state. |
| Tier 2 | Bounded peer freshness work | May emit revision notices or cache-import events. It must not silently rewrite a pack or search result already returned to the caller. |

Remote records are imported evidence, not local truth. They carry origin,
producer, peer identity, origin workspace, trust lane, policy decision,
redaction posture, and import ledger provenance.

## Starter Profiles

Start from the narrowest useful profile. Widen only after reviewing a
share-preview or lane-grant preview.

| Profile | Allowed lanes | Typical use |
| --- | --- | --- |
| Local only | none | Default for ordinary work and CI. |
| Metadata only | metadata, revisionNotice | Discover that a peer may have relevant evidence without sharing bodies. |
| Redacted body | metadata, revisionNotice, body with `redaction=redact` | Share bounded redacted excerpts after consent. |
| Graph hints | metadata, graphLink, revisionNotice | Let graph structure influence local inspection without body transfer. |
| Full peer body | body with `redaction=share` | High-trust operator-owned nodes only, after explicit review. |

Embedding and search-surrogate lanes are opt-in. Treat them as sensitive even
when memory bodies are denied, because they can still leak semantic information.

## Single-Machine Baseline

For a single machine, keep mesh off and use ordinary commands:

```bash
ee status --workspace . --json
ee pack "prepare release" --workspace . --max-tokens 4000 --json
```

`ee status` should not report mesh as a required capability. If a build emits a
mesh degraded code for these local-only commands without an explicit mesh flag
or mesh-enabled config, treat it as a regression against ADR 0037.

## Two-Machine Metadata-Only Setup

Use a metadata-only profile before granting body or embedding lanes. Materialize
the conservative peer group first, then select the opaque enrolled `peerId`
from the peer list. Raw Tailscale node keys remain transport/discovery inputs;
they are not the public lane-consent target.

```bash
ee mesh status --workspace . --json
ee mesh discovery-policy --workspace . --json
ee mesh auto-enroll --workspace . --dry-run --json
ee mesh auto-enroll --workspace . --json
ee mesh peers --workspace . --json

PEER_ID=peer_example123
ee mesh preview-grant "$PEER_ID" --lane metadata --workspace . --json
ee mesh preview-grant "$PEER_ID" --lane revision-notice --workspace . --json
```

The policy should deny bodies and embeddings until a later explicit review.
See `docs/mesh/peer_policy.md` for the `[[mesh.peer_policies]]` fields and the
`tests/fixtures/mesh/peer_policy_metadata_only.json` fixture.

## Machine-Readable Examples

These examples show the live fields an operator should inspect before widening
a lane. The ordinary preview is deterministic, token-free, and does not mutate
policy or append an audit row. A body-lane preview under the metadata-only
starter profile has this shape:

```jsonc
{
  "schema": "ee.response.v2",
  "success": true,
  "data": {
    "command": "mesh preview-grant",
    "workspaceId": "wsp_local_release",
    "preview": {
      "schema": "ee.mesh.lane_grant_preview.v2",
      "copyVersion": "ee.mesh.lane_grant_preview.copy.v1",
      "workspaceId": "wsp_local_release",
      "target": {
        "adapterVersion": "ee.mesh.grant_target.v1",
        "peerId": "peer_example123"
      },
      "lane": "body",
      "grantGeneration": 2,
      "currentPolicy": {
        "generation": "policy_current_2",
        "lane": "body",
        "decision": "deny"
      },
      "proposedPolicy": {
        "generation": "policy_proposed_3",
        "lane": "body",
        "decision": "allow"
      },
      "candidateSet": [
        { "memoryId": "mem_example", "revisionId": "rev_example" }
      ],
      "affectedMemoryCount": 1,
      "redactedFromExposureCount": 0,
      "previewSampleStrategy": "random",
      "previewSampleLimit": 25,
      "previewSample": [
        {
          "memoryId": "mem_example",
          "revisionId": "rev_example",
          "level": "procedural",
          "kind": "rule",
          "contentPreview": "[redacted preview]",
          "tags": ["release"],
          "trustClass": "agent_validated",
          "hasSensitiveTags": false,
          "redactedFields": ["content"],
          "wouldExposeUnderProposedPolicy": true
        }
      ],
      "redactionRulesApplied": ["secret_detector"],
      "cautionCodes": ["redaction_active"],
      "cautions": [
        {
          "kind": "redaction_active",
          "message": "One or more preview rows have fields removed by redaction.",
          "severity": "info"
        }
      ]
    }
  },
  "degraded": []
}
```

`candidateSet[]` binds every revision considered, while `previewSample[]` is
only the bounded, already-redacted operator view. Inspect the target, both
policy generations, affected/redacted counts, exact sample, and cautions.

Human grant mode renders that same canonical preview and asks `y/N`. For a
non-interactive JSON grant, explicitly request the 15-minute sensitive bearer
and pipe only the bearer field into bounded stdin:

```bash
ee mesh preview-grant "$PEER_ID" --lane body --workspace . \
  --issue-approval-token --json \
  | jq -r '.data.preview.approvalToken.bearer' \
  | ee mesh grant "$PEER_ID" --lane body --workspace . \
      --preview-token-stdin --json
```

Never put the bearer in arguments, a shell variable, logs, support bundles, or
audit notes. Verification authenticates it before revealing comparison detail;
then snapshot comparison, generation compare-and-swap, allow, and audit commit
atomically. An expired or drifted authenticated bearer returns
`mesh_approval_token_stale`; malformed, tampered, wrong-context, or wrong-key
input returns `mesh_approval_token_invalid` without an authentication oracle.

## Revisable Pack Flow

Use revisable mode when a caller wants an immediate local answer but also wants
to know whether peer freshness may change the answer later.

```bash
ee pack "audit release readiness" --workspace . --mesh revisable --json
```

Expected behavior:

- The pack remains immediately usable.
- Cached peer material can appear only after authorization and provenance
  checks.
- Revision tokens or revision notices are explicit fields, not hidden mutation.
- A later re-query may produce a new pack hash, but the original pack remains
  explainable from its original local and cached inputs.

If a command silently waits on peers or changes an already emitted pack without
a revision token, it violates the command-mode contract in
`docs/mesh/command_modes.md`.

A revisable response should make peer freshness explicit without blocking local
use of the pack:

```jsonc
{
  "schema": "ee.response.v2",
  "success": true,
  "data": {
    "pack": {
      "schema": "ee.pack.v2",
      "hash": "pack_...",
      "mesh": {
        "mode": "revisable",
        "tier1Usable": true,
        "revisionToken": "mesh_rev_2026_05_19_001",
        "peerFreshness": {
          "status": "stale",
          "peerAlias": "builder-host",
          "materialLane": "revisionNotice",
          "bodyFetchAllowed": false
        }
      }
    }
  },
  "degraded": []
}
```

This means the caller has a valid local pack and a redaction-safe reason to
re-query later. It does not mean peer bodies, embeddings, or raw artifact paths
were exported.

## Trust And Redaction Rules

Operators should assume peer material is less trusted than local explicit
memory unless policy and outcome evidence prove otherwise.

| Rule | Operational meaning |
| --- | --- |
| Remote material never imports as `human_explicit` | A remote human claim maps to peer evidence locally. |
| Body and embedding lanes are separate grants | Metadata approval does not imply body or embedding approval. |
| Default action is deny | Missing policy should block or quarantine, not infer trust. |
| Redaction-safe aliases leave the policy layer | Raw peer paths, policy file paths, bodies, embeddings, and secrets stay out of status and support bundles. |
| Withdrawal is best effort outside the local node | `ee` can tombstone or revoke local cache and emit audit evidence, but it cannot force unaudited remote copies to disappear. |

Before widening a lane, run a preview and inspect the exact decision surface:

```bash
ee mesh preview-grant <peer-id> --lane body --workspace . --json \
  | jq '.data.preview | {target, lane, grantGeneration, currentPolicy, proposedPolicy, affectedMemoryCount, redactedFromExposureCount, cautionCodes}'
```

Use `ee mesh peers --workspace . --json` to obtain the opaque `peerId`. Do not
substitute a discovery node key into the lane-consent command.

## Safe Profile Promotion

Treat sharing profiles as a ladder, not a switch. A profile should move to the
next rung only when the current rung has produced useful evidence and no
unexpected peers, workspaces, or degraded codes.

Starter profiles are intentionally narrow and inspectable:

| Profile ID | Shape | Default use |
| --- | --- | --- |
| `starter.metadata_only` | Metadata lane only; no evidence refs, bodies, or embeddings | First profile for a newly enrolled peer. |
| `starter.evidence_refs` | Metadata plus evidence-reference lane; no bodies or embeddings | Use only when the peer needs provenance pointers and the artifact paths are redaction-safe aliases. |
| `starter.trusted_bodies` | Metadata, evidence refs, and body lane; embeddings remain local | Use only for reviewed or validated memories shared with an owned/trusted peer. |

Every profile preview should log `profile_id`, `candidate_count`,
`allowed_count`, `denied_count`, and `deny_reason` before a peer receives new
material. Deny rows are not errors; they are the audit trail explaining why a
memory, body, evidence reference, or embedding surrogate did not sync.

| From | To | Required evidence before promotion |
| --- | --- | --- |
| Local only | Metadata only | `ee mesh status --json` names only expected peers and workspaces. |
| Metadata only | Graph hints | Preview shows `graphLink` is allowed or quarantined without body or embedding export. |
| Metadata only | Redacted body | Preview sample is small, consented, redacted, and tied to safe peer/workspace aliases. |
| Redacted body | Full peer body | Operator owns both nodes, audit trail is intact, and the exact body lane grant is intentional. |
| Any profile | Embedding/search surrogate | Treat as high-sensitivity semantic export; require a separate privacy review. |

Rollback should be practiced before widening a lane. If the operator cannot
explain how to disable mesh, revoke a peer, and inspect the audit timeline, the
profile is not ready for body, embedding, or search-surrogate lanes.

## Incident Containment

Use containment when a peer appears unexpected, drift crosses the documented
thresholds, a policy decision surprises the operator, or an agent reports a
body/embedding export that was not explicitly approved.

Start with read-only evidence:

```bash
ee mesh status --workspace . --json
ee mesh status --workspace . --refresh --json
ee mesh status --workspace . --explain-peer <node-key> --json
ee audit verify --workspace . --json
```

Then choose the narrowest mutating action that matches the incident:

| Situation | First action | Why |
| --- | --- | --- |
| Unknown or wrong workspace peer | `ee mesh disable --workspace . --dry-run --json` | Shows rollback impact before changing peer-group rows. |
| One peer should stop receiving all material | `ee mesh peer revoke <peer-id> --workspace . --json` | Preserves the rest of the peer group when only one peer is wrong. |
| One lane was too broad | `ee mesh revoke-lane <peer-id> --lane <lane> --workspace . --json` | Narrows only that lane and invalidates every preview from the prior generation. |
| Tailnet or node-key changed | `ee mesh disable --reason "tailnet or node-key changed" --workspace . --json` | Backup-restored or tailnet-changed identity must fail closed. |
| Audit row write fails | Stop before enrollment or grant changes | Mesh state must not change without its forensic precursor row. |
| Body or embedding lane was too broad | Run `ee mesh revoke-lane`, then review audit and cache rows | Withdrawal stops future serving but cannot erase remote cached or copied bytes. |

Do not destroy caches or rewrite Git state during containment. Preserve evidence
first; later cleanup belongs to the owning recovery bead or an explicit human
operation.

## Audit And Forensics

Mesh operations should leave enough redaction-safe evidence for an operator to
answer three questions: what was attempted, what policy decided, and what local
state changed.

Use the audit timeline for materialized changes:

```bash
ee audit timeline \
  --event-type mesh.auto_enrollment_intended \
  --workspace . \
  --json
```

For a support handoff, include only:

- command name and mode, not raw memory bodies;
- peer/workspace aliases, not raw peer paths or local host paths;
- policy code, schema id, fixture id, and summary hash;
- whether body, embedding, graphLink, or revisionNotice lanes were allowed;
- whether the operation was dry-run, audit-only, applied, revoked, or disabled.

If the audit chain itself is degraded, treat mesh as blocked for any widening
action. Existing local-only `ee` commands remain valid while audit repair is
investigated.

## Restore And Disaster Recovery Semantics

A restored database can be correct storage-wise but wrong identity-wise. The
mesh layer must treat restored-on-another-machine and tailnet-change cases as
high-risk until the operator explicitly re-enrolls.

Operational rules:

- If `selfNodeKey`, tailnet id, workspace binding, or peer-group hash differs
  from the stored materialization, block auto-enrollment and lane widening.
- Run `ee mesh disable --dry-run --workspace . --json` before mutating restored
  mesh state.
- Prefer metadata-only re-enrollment after restore, even when the old machine
  previously had body lanes.
- Do not reuse old body, embedding, or search-surrogate grants just because the
  backup restored them.
- Record restore context in the audit reason, for example
  `--reason "restored from backup onto replacement machine"`.

Backups and restores are local durability operations; they do not prove that
remote peers purged cached material. Any closeout must keep that limitation
visible.

## Peer Throttling And Resource Isolation

Mesh should not let a noisy peer make local `ee pack`, `ee search`, or
`ee why` feel slower or less deterministic. Operators should prefer backpressure
and quarantine over broad blocking mode.

| Signal | Meaning | Response |
| --- | --- | --- |
| Peer repeatedly times out | Transport or responder is slow | Keep local Tier 1 usable; avoid blocking mode. |
| Peer sends too much material | Resource isolation is working only if it throttles or quarantines | Keep metadata-only until rate limits are tuned. |
| Peer causes audit backpressure | Forensics path is overloaded | Stop widening lanes; inspect audit health before retrying. |
| Peer returns low-quality evidence | Trust decay should lower influence | Keep material as peer evidence, not local truth. |
| Many agents poll mesh status | Coordination issue, not a reason to widen mesh | Use cached status, Agent Mail, or Beads handoffs instead of poll storms. |

If the command supports a blocking mesh mode in the future, reserve it for
explicit operator-initiated freshness checks with a hard latency budget. Default
agent workflows should stay `off`, `cache`, or `revisable`.

## Diagnosing Stale Or Unreachable Peers

Start with read-only status surfaces:

```bash
ee mesh status --workspace . --json
ee mesh status --workspace . --refresh --json
ee mesh status --workspace . --explain-peer <node-key> --json
ee doctor --workspace . --json
```

Interpret common outcomes this way:

| Symptom | Meaning | Next action |
| --- | --- | --- |
| Mesh unavailable on a local-only command | Likely a regression | File or update the relevant SRR6 bead; do not require mesh for local workflows. |
| Peer unreachable | Transport or responder not reachable | Check the peer, but keep local commands usable. |
| Workspace mismatch | Peer is running `ee` for another workspace | Do not import; use explicit enrollment only if intended. |
| Policy denied | Local policy did its job | Review lane grants and redaction posture before widening. |
| Stale revision token | Local answer may be behind peer cache | Re-query after sync or inspect revision ancestry. |
| Body fetch denied | Metadata may be available, body is not | Do not treat the pack as missing local evidence; request a grant only if needed. |

No troubleshooting step should suggest deleting cache directories, resetting
Git state, stashing, checking out another ref, creating a worktree, or running
local Cargo as proof.

## Degraded And Policy Codes

Mesh has two related but different code surfaces:

| Surface | Example code | Meaning | Operator response |
| --- | --- | --- | --- |
| `degraded[]` | `discovery_policy_no_ee_mesh_tag` | Status or discovery could not fully satisfy an optional mesh condition. | Follow the repair action or keep the command in local-only mode. |
| `degraded[]` | `tailscale_shields_up` | Transport is reachable enough to diagnose, but inbound mesh traffic is blocked. | Decide whether this node should accept mesh traffic before changing Tailscale settings. |
| Error | `mesh_approval_token_invalid` (high) | Bearer authentication failed, including malformed, tampered, wrong-context, or non-current-key input. | Discard it; review a new preview and explicitly issue a fresh bearer. |
| Error | `mesh_approval_token_stale` (warning) | The bearer authenticated, but expired or no longer matches the current canonical snapshot/generation. | Review the new preview before issuing another bearer. |
| Error | `mesh_store_authentication_unavailable` (high) | The store-auth root cannot authenticate approvals. | Run `ee doctor --json`; do not widen a lane until repaired. |
| Policy failure surface | `mesh_peer_policy_denied` | Inbound peer material was denied by local policy. | Keep the denial unless a human approves a narrower grant. |
| Policy failure surface | `mesh_peer_policy_quarantined` | Inbound material was retained only as quarantined evidence. | Inspect the audit and quarantine reason before using it in packs. |
| Policy failure surface | `mesh_outbound_policy_denied` | Local policy refused to export a requested payload. | Do not bypass redaction; preview a safer lane or leave sharing disabled. |

When adding Beads or Agent Mail notes, cite only these code names, safe aliases,
fixture names, and schema names. Do not paste raw peer stderr, memory bodies,
embeddings, raw workspace paths, policy file paths, or secret-scan excerpts into
coordination channels.

## Agent Closeout Language

When reporting mesh work in Beads or Agent Mail, be precise:

```text
Mesh status checked in <mode>. Local Tier 1 result remained usable.
Peer freshness: <none | revision token emitted | stale | unreachable>.
Policy posture: <metadata-only | redacted body | denied | quarantined>.
No body or embedding material was exported unless explicitly granted.
This is not proof that remote peers deleted or forgot prior unaudited copies.
```

For blocked or degraded mesh work, say what did not happen:

```text
Mesh peer material was not imported because policy returned <code>.
The command did not run in blocking peer mode.
Local ee workflows remain valid without mesh.
```

## Related Documents

- `docs/adr/0037-optional-mesh-memory.md` - design invariants and threat model.
- `docs/mesh/command_modes.md` - `off`, `cache`, `revisable`, and `blocking`
  command contracts.
- `docs/mesh/peer_policy.md` - trust lanes, material lanes, redaction, and
  body-fetch policy.
- `docs/mesh/workspace_scope.md` - workspace and peer-group isolation.
- `docs/mesh/verification_matrix.md` - proof rows and structured e2e logging
  for SRR6 slices.
- `docs/agent-ux/auto_enrollment_onboarding.md` - detailed agent workflow for
  auto-enrollment, drift, disable, and lane-preview surfaces.
