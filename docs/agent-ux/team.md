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
`ee team doctor` also reports `admission`, `key_store`, `broker_port`,
`client_only`, `whois`, and `body_cache_lifecycle`. Staging or
`invalidated_pending_purge` rows are a warning, not a successful fetch.

## Mutations

- `ee team create`, `invite`, `join`, `share bodies --confirm`,
  `unshare`, `leave`, `pause`, `resume`, `idp set`, `idp attest`, and
  `daemon install --confirm` are durable writes.
- Default `ee team share bodies` is a **preview**. It must stay
  token-free and must not publish cache bytes.
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
