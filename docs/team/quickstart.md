# Team Confederation Quickstart

Personas: Hana (origin, trusted teammate) and Priya (joiner on a second
Unix host). Both already run local `ee`. Neither command below requires
the daemon. JSON is the agent-facing surface.

## Cold path

```bash
# Hana — create the origin team and mint a one-use invite
ee team create --name "Analysts" --workspace . --json
ee team invite --workspace . --json

# Priya — join over live TCP using the invite code
ee team join --code "$INVITE" --endpoint "$HANA_ADDR" --workspace . --json

# Either side — status, members, health
ee team status --workspace . --json
ee team members list --workspace . --json
ee team doctor --workspace . --json
```

Share metadata first. Bodies are a separate, confirm-gated publication.

```bash
# Hana — preview is token-free and deterministic
ee team share history --workspace . --json
ee team share bodies --workspace . --json

# Hana — publish only after preview; consume a robot token via stdin
ee team share bodies --issue-token --workspace . --json
ee team share bodies --confirm --token-stdin --workspace . --json

# Priya — fetch a published body through the authenticated session
ee team fetch body --key "$CACHE_KEY" --workspace . --json
```

Pause fences network exchange without deleting membership. Resume
reopens it. Leave is a self-removal, not a tombstone of origin history.

```bash
ee team pause --confirm --workspace . --json
ee team resume --confirm --workspace . --json
ee team leave --confirm --workspace . --json
```

## Identity (optional)

Tailnet-attested membership is the default trusted-team arm. OIDC
device-code is the secretless-public-client arm.

```bash
ee team idp require --tailnet-attested --workspace . --json
ee team idp status --workspace . --json
ee team idp device --execute --ca-bundle "$PINNED_CA" --workspace . --json
ee team idp attest --id-token - --jwks-url "$JWKS" --ca-bundle "$PINNED_CA" --workspace . --json
```

## Operations

Foreground steward is enough for a two-node lab. User-scoped install is
optional and never requires root.

```bash
ee team steward once --workspace . --json
ee daemon install --confirm --json          # write launchd / systemd --user unit
ee daemon install --confirm --load --json   # also load the supervisor
```

## Custom port and multi-user limits

- The origin responder owns **one** TCP port for every registered
  workspace on the host. A second user cannot bind a second responder.
- Override the port only through the documented mesh config; do not
  start a second `ee` listener by hand.
- Windows remains client-only: team credentials and body publication
  fail closed with `mesh_key_store_unavailable` /
  `mesh_body_cache_lifecycle_failed` until the SID/DACL adapter lands.

Repair commands always come from `ee team doctor --json`.
