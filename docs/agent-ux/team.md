# Agent notes: `ee team`

Use `ee team` only when two or more trusted Unix `ee` nodes must share
origin-owned memory. Local `ee pack` / `ee search` stay the default.

## Safe first commands

```bash
ee team status --workspace . --json
ee team doctor --workspace . --json
ee team members list --workspace . --json
```

Treat any `degraded[]` or doctor `error` as a stop. Repair strings on
those surfaces are the next command, not a prompt to invent one.
`ee team status` admission fields include `peerSnapshotCount`,
`throttledPeerCount`, `budgetExhaustedPeerCount`, and
`coalescedExhaustion` from the last persisted broker snapshot.
`data.budgets` is the T6.5 join/relay/body/index profile
(`ee.team.budgets.v1`).
`ee team doctor` also reports `admission`, `key_store`, `broker_port`,
`client_only`, `whois`, `body_cache_lifecycle`, `index_rematerialization`,
`origin_outbox`, `invite_auth_floor`, `pending_invites`,
`delegated_members`, `signing_rotation`, `pair_rotation`, `projects`,
and `removal_acknowledgements`. Staging, `invalidated_pending_purge`,
pending index jobs, or behind/blocked/quarantined peer cursors are a
warning, not a successful fetch. A signed removal seeds a durable
audience matrix; pending acknowledgements are a warning and fanout is
not bounded until those members apply the event. Repair is
`ee team steward once`. Pending invites created before the authorization
floor are an error; the repair is `ee team revoke --all-before-floor`.

## Mutations

- `ee team create`, `invite`, `join`, `share bodies --confirm`,
  `unshare`, `leave`, `pause`, `resume`, `idp set`, `idp attest`, and
  `daemon install --confirm` are durable writes. `ee team invite`
  defaults the locator to the local Tailscale IPv4 address; pass
  `--endpoint` when Tailscale is absent. `ee team status` lists
  `pendingInvites[]` for `ee team revoke --invite-id` and
  `pendingRemovalAcks[]` for unsigned removal fanout. Origin project
  shares rematerialize with `ee team projects reconcile`; adopt binds
  a local path afterward.
- Default `ee team share bodies` is a **preview**. It must stay
  token-free and must not publish cache bytes. `--representation
  already_redacted` is allowed; redact-over-exact is refused.
- Robot confirm consumes `--token-stdin`. Never put an `eeap1_` bearer
  on the argv, in env, or in a support bundle.
- `ee team idp device --execute` uses constrained HTTPS (absolute curl,
  `--proto =https`, cleared env, optional `--cacert`). Do not add
  `--insecure`.

## Fail-closed codes

| Code | Meaning | Next |
| --- | --- | --- |
| `mesh_key_store_unavailable` | No reviewed secure-file adapter | Use Unix or Windows with the hardened adapter |
| `mesh_body_cache_lifecycle_failed` | Body publication could not prove T2.1 | Retrieval stays metadata-only |
| `mesh_approval_token_invalid` | Wrong token/MAC/workspace | Fresh preview; do not retry the old bearer |
| `mesh_approval_token_stale` | Authentic but expired/drifted | Fresh preview |
| `mesh_peer_throttled` / `mesh_payload_rejected` | Admission refused the peer | Wait or shrink the batch |

## Do not

- Do not start a second responder port for a second workspace.
- Do not treat Tailscale WhoIs as team membership.
- Do not claim Windows DACL/reparse parity; it is fail-closed.
- Do not close a team-confed bead because a doctor check is green.
- Do not delete body-cache files. Unshare invalidates; reconcile
  never resurrects from the filesystem.
