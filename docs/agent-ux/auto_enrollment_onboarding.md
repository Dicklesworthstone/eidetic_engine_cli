# Agent Onboarding: Auto-Enrolled Mesh

This guide is for coding agents that consume the `ee mesh` surface after
`ee mesh auto-enroll` has materialized a peer-group binding. Treat the JSON
schemas as the contract; the commands below are the inspection and repair
surfaces. These tools coordinate access to remote ee memory; they do not
replace `ee context`, `ee search`, or `ee why` — those still drive the
local-first retrieval that mesh accelerates.

## TL;DR

If `tailscaled` is running and authenticated, and at least one peer on the
same tailnet also runs ee with mesh enabled, then this is the full flow:

```bash
ee mesh auto-enroll --workspace . --json
```

Materializes the peer-group binding in one command. Idempotent — re-running
returns `auto_enrollment_already_complete` (info). Reversal:

```bash
ee mesh disable --workspace . --json
```

Read-only inspection (never writes):

```bash
ee mesh status --workspace . --json
ee mesh status --workspace . --refresh --json   # bypass discovery cache
ee mesh status --workspace . --explain-peer <node-key> --json
```

## Required Preconditions

Before auto-enroll can succeed:

1. **`EE_MESH_ENABLED=1`** in the user's environment (default is `0`; with
   `0`, every mesh code path is dormant and ordinary `ee` commands are
   byte-stable).
2. **`tailscaled` running and authenticated.** Check via:
   ```bash
   ee status --workspace . --json \
     | jq '.data.mesh.tailscale | {installed, daemonReachable, authenticated, tailnetId}'
   ```
   Repair via `tailscale up` if `authenticated` is false.
3. **At least one peer on the same tailnet that runs ee.** Check via:
   ```bash
   ee mesh status --workspace . --json \
     | jq '.data.autoEnrollment.discovery | {eligiblePeerCount, eeCapablePeers}'
   ```
   If `eligiblePeerCount = 0`, run ee on a second host first.
4. **The `ee daemon` process running** (so the local hello responder accepts
   inbound discovery from peers). Check via:
   ```bash
   ee mesh hello-responder status --workspace . --json | jq '.data.running'
   ```
   Repair: `ee daemon --foreground`.

When any of these is missing, `ee mesh status` surfaces a degraded code with
the literal repair command (see "Repair Actions" below).

## Response Envelope Contract

All `ee mesh *` commands emit the standard `ee.response.v2` envelope:

```jsonc
{
  "schema": "ee.response.v2",
  "success": true,
  "data": { /* command-specific schema, see table */ },
  "degraded": [ /* zero or more { code, severity, message, repair } entries */ ]
}
```

Error path uses `ee.error.v2`:

```jsonc
{
  "schema": "ee.error.v2",
  "success": false,
  "error": {
    "code": "...",
    "severity": "info" | "low" | "warning" | "medium" | "high" | "critical",
    "message": "...",
    "repair": "...",
    "details": { "recovery": [ /* structured actions */ ] }
  }
}
```

Agents should always check `degraded[]` even when `success=true`; many mesh
states are degraded-but-actionable (e.g. drift available, responder offline,
tailscale shields-up).

## Per-Command Cheat Sheet

| Command | `data.schema` | Mutates state? |
|---|---|---|
| `ee status --json` | `ee.status.v1` (with `mesh.tailscale` block) | no |
| `ee mesh status --json` | `ee.mesh.status.v1` (with `autoEnrollment` block) | no |
| `ee mesh status --refresh --json` | `ee.mesh.status.v1` (cache bypassed) | no |
| `ee mesh status --explain-peer <key> --json` | `ee.mesh.peer_state.v1` | no |
| `ee mesh auto-enroll --dry-run --json` | `ee.mesh.auto_enrollment_result.v1` | no |
| `ee mesh auto-enroll --json` | `ee.mesh.auto_enrollment_result.v1` | YES (after audit row) |
| `ee mesh auto-enroll --include <key>* --exclude <key>* --json` | same | YES |
| `ee mesh auto-enroll --explain --json` | decision-tree view, no envelope mutation | no |
| `ee mesh auto-enroll --replace-manual-with-auto --json` | same as auto-enroll | YES (migration audit row) |
| `ee mesh disable --workspace . --json` | `ee.mesh.disable_result.v1` | YES (rollback audit row) |
| `ee mesh disable --dry-run --json` | same | no |
| `ee mesh revoke <node-key> --json` | `ee.mesh.revoke_result.v1` | YES (per-peer + denylist) |
| `ee mesh hello-responder status --json` | `ee.mesh.hello_responder.status.v1` | no |
| `ee mesh steward status --json` | `ee.mesh.steward.status.v1` | no |
| `ee mesh steward run-now --json` | same | YES (when steward enabled) |
| `ee mesh discovery-policy --json` | `ee.mesh.discovery_policy.v1` | no |
| `ee mesh discovery-policy set --mode <m>` | same | YES (writes `.ee/config.toml`) |
| `ee mesh discovery-policy allow <node-key>` | same | YES (writes allowlist) |
| `ee mesh discovery-policy deny <node-key>` | same | YES (writes denylist) |
| `ee mesh preview-grant <node-key> --lane <lane> --json` | `ee.mesh.lane_grant_preview.v1` | no |
| `ee doctor --json` | `ee.doctor.v1` (with `categorized.mesh_auto_enroll` block) | no |

## The Status Surface

`ee mesh status --json` returns the comprehensive picture. The
`data.autoEnrollment` block looks like this:

```jsonc
{
  "autoEnrollment": {
    "schema": "ee.mesh.auto_status.v1",
    "tailscale": { /* ee.tailscale.local.v1: authenticated, tailnetId, ... */ },
    "helloResponder": { "running": true, "listenAddress": "100.64.0.5:41888", ... },
    "discovery": {
      "tailnetId": "tn_...",
      "probedPeerCount": 5,
      "eligiblePeerCount": 3,
      "eeCapablePeers": [ /* node entries */ ],
      "skippedPeers": [ /* node entries with reason */ ]
    },
    "discoveryCache": { "cachedAt": "...", "validUntil": "...", "hit": true },
    "materialized": {
      "peerGroupId": "pg_01HQX...",
      "peerSetHash": "blake3:...",
      "peerCount": 3,
      "lanePolicy": {
        "metadata": "allow",
        "revisionNotice": "allow",
        "curationSignal": "allow",
        "body": "deny",
        "embedding": "deny",
        "graphLink": "deny"
      },
      "boundTailnetId": "tn_...",
      "materializedOnNodeKey": "nodekey:...",
      "enrollmentSource": "auto"
    },
    "peerStateBreakdown": { "active": 3, "softStale": 0, "hardStale": 0, "denylisted": 0 },
    "drift": {
      "newPeersAvailable": [],
      "stalePeersInConfig": [],
      "transientUnreachable": [],
      "tailnetChanged": false,
      "nodeKeyChanged": false,
      "manualConflictPresent": false,
      "driftSeverity": "none",
      "actionGraph": { /* ee.repair_action_graph.v1, see "Repair Actions" */ },
      "nextActionHint": null
    },
    "stewardPosture": { "enabled": false, "lastReconciliationAt": null, ... },
    "degraded": [ ]
  }
}
```

### Reading the drift block

Drift severity classification (locked by tests):

| Severity | Trigger | Agent action |
|---|---|---|
| `none` | Discovery matches materialized exactly; no soft-stale peers | No-op |
| `info` | ≤2 new peers OR `transientUnreachable[]` non-empty (soft-stale) | Surface to user; consider re-checking on next idle tick |
| `warning` | >2 new/stale peers OR `hard_stale` peers OR tailnet display name changed | Surface to user with `actionGraph` repair |
| `medium` | `tailnetChanged=true` OR `nodeKeyChanged=true` OR `manualConflictPresent=true` OR `helloResponder.running=false` | Block further auto-enroll; surface refusal + repair |

`transientUnreachable[]` peers have missed 1 probe but are within the
1-hour grace window. **Do not** treat them as removed. They will heal
automatically if they come back, and will escalate to `stalePeersInConfig`
only if the misses persist beyond the hard threshold.

### Reading the actionGraph

When `drift.actionGraph` is non-empty, it carries
`ee.repair_action_graph.v1`:

```jsonc
{
  "schema": "ee.repair_action_graph.v1",
  "actions": [
    {
      "id": "ee_daemon_start",
      "kind": "shell_command",
      "command": "ee daemon --foreground",
      "humanReadable": "Start the ee daemon to enable inbound discovery",
      "prerequisites": ["tailscale_up"],
      "expectedOutcome": {
        "resolvesChecks": ["hello_responder_running"],
        "preconditionsForNextActions": ["ee_mesh_auto_enroll"]
      },
      "priority": "high",
      "estimatedDurationSeconds": 10,
      "reversible": true,
      "reversalCommand": null,
      "requiresUserConfirmation": false,
      "executionContext": "user_shell"
    }
  ],
  "topologicallyOrderedExecution": ["tailscale_up", "ee_daemon_start", "ee_mesh_auto_enroll"],
  "parallelizableGroups": [ ["tailscale_up"], ["ee_daemon_start"], ["ee_mesh_auto_enroll"] ],
  "estimatedTotalDurationSeconds": 30
}
```

Walk `topologicallyOrderedExecution` to execute actions in
dependency-correct order. Use `parallelizableGroups` if you want to fan
out independent branches.

`ee doctor --json` returns the same schema (`ee.doctor.action_graph.v1`
wrapper around `ee.repair_action_graph.v1`) but for the full 15-check
posture, not just drift. Use it when you want the comprehensive picture.

## Auto-Enroll Flow

`ee mesh auto-enroll --workspace . --json` does this (errors fail-closed at
every step):

1. Acquire workspace write-owner lock. Conflict → `auto_enrollment_concurrent_attempt`.
2. Fresh probe (bypasses cache).
3. Tailnet-change check (SRR6.46.8). Mismatch → `auto_enrollment_tailnet_changed`.
4. Manual-config conflict check. Present → `auto_enrollment_manual_config_present`.
5. Autodiscovery (forced refresh).
6. Apply `--include` / `--exclude` overrides.
7. Compute intended config + peer-set hash.
8. Idempotence check. Match → `auto_enrollment_already_complete`.
9. Emit `ee.mesh.auto_enrollment_summary.v1` audit row.
   **If this fails → `auto_enrollment_audit_failed` (critical) → no peer-group write.**
10. Materialize via SRR6.24 + SRR6.30 + SRR6.5 in one DB transaction.
11. Kick `ee mesh sync-once` (best-effort).
12. Return `ee.mesh.auto_enrollment_result.v1`.
13. Release lock.

### Common Degraded Codes

| Code | Severity | When | Repair |
|---|---|---|---|
| `tailscale_not_installed` | warning | No `tailscale` binary and no local socket | `brew install tailscale` / `sudo apt install tailscale` |
| `tailscale_daemon_unreachable` | warning | Daemon not responding | `sudo systemctl status tailscaled` |
| `tailscale_not_authenticated` | warning | Not logged in to a tailnet | `tailscale up` |
| `tailscale_binary_inauthentic` | high | `--version` output doesn't match Tailscale Inc. format | `which tailscale` + verify provenance + reinstall |
| `tailscale_shields_up` | warning | shields-up is on; inbound blocked | `tailscale set --shields-up=false` |
| `tailscale_probe_timeout` | warning | Probe hit the 1500ms budget | Set `EE_TAILSCALE_PROBE_TIMEOUT_MS=<larger>` |
| `no_ee_peers_on_tailnet` | info | Tailnet healthy, no other ee instances | Run ee on a second tailnet host |
| `peer_discovery_workspace_mismatch` | info | Peers run ee but for a different workspace | (Optional) explicit `ee mesh enroll <node-key>` |
| `hello_responder_not_running` | medium | `ee daemon` not running; peers cannot reach us | `ee daemon --foreground` |
| `hello_responder_port_in_use` | high | Configured port held by another process | `EE_MESH_HELLO_PORT=<other> ee daemon --foreground` |
| `auto_enrollment_no_eligible_peers` | info | Discovery returned zero eligible peers | (See discovery hints) |
| `auto_enrollment_partial_failure` | warning | Some peer enrollments succeeded, some failed; transaction rolled back | Re-run; check per-peer details |
| `auto_enrollment_blocked_by_policy` | medium | SRR6.5 trust policy rejected the auto defaults | Manual `ee mesh enroll` |
| `auto_enrollment_already_complete` | info | Peer set matches existing materialization | No-op |
| `auto_enrollment_concurrent_attempt` | warning | Another agent holds the write-owner lock | Wait, then retry |
| `auto_enrollment_tailnet_changed` | medium | Bound tailnet differs from current | `ee mesh disable && ee mesh auto-enroll` |
| `auto_enrollment_node_key_changed` | medium | DB likely restored from a different machine | `ee mesh disable --reason "restored from different machine" && ee mesh auto-enroll` |
| `auto_enrollment_manual_config_present` | medium | Manual peer-group exists; auto refuses to overwrite | `ee mesh auto-enroll --replace-manual-with-auto` |
| `auto_enrollment_audit_failed` | critical | SRR6.46.5 audit-row write failed; fail-closed | Inspect audit chain integrity (`ee audit verify --json`) |
| `auto_enrollment_sync_once_failed` | warning | Materialization OK; post-kick sync-once failed | Retry `ee mesh sync-once` |
| `auto_enrollment_invalid_override_node_key` | warning | `--include`/`--exclude` named a node-key not on tailnet | Confirm node-key spelling |
| `mesh_disable_noop` | info | No materialized peer-group to disable | No-op |
| `mesh_disable_concurrent_attempt` | warning | Another agent holds the write-owner lock | Wait, then retry |
| `mesh_revoke_unknown_peer` | warning | Named peer not in current peer-group | Re-list eligible peers |
| `discovery_policy_no_ee_mesh_tag` | info | Responder is in `service_tag` mode but not advertising the tag | `tailscale up --advertise-tags=tag:ee-mesh` |
| `lane_grant_preview_peer_not_in_group` | warning | Named peer not in the workspace's auto-enrolled group | Preview still runs; peer wouldn't receive until enrolled |
| `steward_auto_enroll_disabled` | info | `EE_MESH_AUTO_ENROLL_ON_DEMAND=0` (default) | Set the env var if you want auto-reconciliation |

## Safety Patterns

### Always preview before granting body/embedding lanes

```bash
ee mesh preview-grant <node-key> --lane body --workspace . --json
```

Returns `ee.mesh.lane_grant_preview.v1` with:

- `affectedMemoryCount` — total memories that become visible.
- `redactedFromExposureCount` — how many of those still have redacted fields.
- `previewSample[]` — sampled memory rows so you can spot-check.
- `cautions[]` — explicit hazards:
  - `high_trust_class_exposure` — `trust_class=human_explicit` memories exposed.
  - `large_volume_exposure` — >1000 memories exposed.
  - `sensitive_tags_in_exposure` — memories tagged `secret` / `private` / `personal` / `internal` exposed.
  - `cross_workspace_overlap` — memories also bound to other workspaces.

Treat any non-empty `cautions[]` as a stop-and-think signal.

### Forensic correlation via summaryHash

Every materialization, rollback, or audit-only event writes an audit row
whose `details.summaryHash` is the canonical `blake3` of the row's payload.
Cross-reference via:

```bash
ee audit timeline --event-type mesh.auto_enrollment_intended --workspace . --json \
  | jq '.[] | select(.summaryHash == "blake3:...")'
```

Rollback rows carry `previousSummaryHash` referencing the materialization
they reversed, so a "what was undone, and when" query is two index lookups.

### Pure-read invariant

`ee mesh status`, `ee mesh hello-responder status`, `ee mesh steward status`,
`ee mesh preview-grant`, and `ee mesh discovery-policy --explain` are all
read-only. They never write peer-group rows, never write audit rows. The
only mutating mesh commands are `auto-enroll`, `disable`, `revoke`,
`discovery-policy set|allow|deny`, and `steward run-now` (when enabled).

If your agent harness needs to poll mesh state between operations, prefer
`ee mesh status --json` (which hits the 30s discovery cache by default).
Add `--refresh` only when you need fresh ground truth.

## Common Workflows

### Walking the action graph from doctor

```bash
ee doctor --workspace . --json \
  | jq -r '.data.categorized.mesh_auto_enroll.actionGraph.topologicallyOrderedExecution[]' \
  | while read action_id; do
      cmd=$(ee doctor --workspace . --json \
        | jq -r ".data.categorized.mesh_auto_enroll.actionGraph.actions[] | select(.id == \"$action_id\") | .command")
      echo "Run: $cmd"
      # Then: bash -c "$cmd" if the agent is allowed to execute
    done
```

### Detecting drift on an idle tick

```bash
severity=$(ee mesh status --workspace . --json | jq -r '.data.autoEnrollment.drift.driftSeverity')
case "$severity" in
  none)    : ;;  # all good, no action
  info)    echo "drift available (info); will check again on next tick" ;;
  warning) echo "drift requires reconciliation"; ee mesh status --workspace . --json | jq '.data.autoEnrollment.drift.actionGraph' ;;
  medium)  echo "drift requires user attention"; ee mesh status --workspace . --json | jq '.data.autoEnrollment.drift.nextActionHint' ;;
esac
```

### Auditing what auto-enroll exposed last week

```bash
ee audit timeline \
  --workspace . \
  --event-type mesh.auto_enrollment_intended \
  --since 7d \
  --json \
  | jq '.[] | {at: .timestamp, peerCount: (.details | fromjson | .intendedPeers | length), outcome: (.details | fromjson | .materializationOutcome), reversal: (.details | fromjson | .reversalCommand)}'
```

## What Mesh Auto-Enrollment Does NOT Do

- It does **not** decide which memories peers can read. SRR6.5 trust + lane
  policy owns that. Conservative defaults (deny body/embedding/graph_link)
  ship out of the box; widening is explicit via `ee mesh grant` after
  reviewing `ee mesh preview-grant`.
- It does **not** sync memories on its own schedule. SRR6.46.14 steward is
  opt-in via `EE_MESH_AUTO_ENROLL_ON_DEMAND=1`. Without that, drift is
  surfaced but not automatically reconciled.
- It does **not** treat tailscale reachability as authorization. A peer is
  on the tailnet AND ee-capable AND in the materialized peer-group AND
  granted the relevant lane before they can read body/embedding data.
- It does **not** mutate retrieval semantics. `ee context`, `ee search`,
  `ee why` continue to return local-first results. Mesh peers' data
  appears as imported evidence with provenance, not as local truth.

## Where to Read More

- ADR 0037: `docs/adr/0037-optional-mesh-memory.md` — the SRR6 mesh umbrella
- ADR 0038: `docs/adr/0038-auto-enrollment-zero-touch.md` — the SRR6.46
  design decisions captured in writing
- `docs/mesh/peer_policy.md` — peer policy + lane semantics
- `docs/mesh/workspace_scope.md` — workspace-scope and namespace isolation
- `docs/schemas/ee.mesh.*.v1.json` — the contract schemas for every command
  on the cheat-sheet table above
- `docs/migration-guide.md` — v0.3.0 migration notes (added when
  bd-36bbk.1.20 lands)
