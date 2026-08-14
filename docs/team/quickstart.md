# Team Confederation Quickstart

Personas: Hana (origin, trusted teammate) and Priya (joiner on a second
Unix host). Both already run local `ee`. Neither command below requires
the daemon. JSON is the agent-facing surface.

## Cold path

```bash
# Hana — create the origin team and mint a one-use invite
ee team create --name "Analysts" --workspace . --json
ee team invite --workspace . --json
# or pin the locator: ee team invite --endpoint "$HANA_ADDR" --workspace . --json
# crash-resume the waiter without re-emitting the secret:
# ee team invite --wait --resume "$INVITE_ID" --workspace . --json

# Priya — join over live TCP using the invite code
ee team join --invite "$INVITE" --workspace . --json

# Either side — status, members, health
ee team status --workspace . --json
ee team members list --workspace . --json
ee team doctor --workspace . --json
# status.pendingInvites[] carries inviteId for ee team revoke --invite-id
```

Share metadata first. Bodies are a separate, confirm-gated publication.

```bash
# Hana — preview is token-free and deterministic
ee team share history --workspace . --json
ee team share bodies --workspace . --json

# Hana — publish only after preview; consume a robot token via stdin
ee team share bodies --issue-token --workspace . --json
ee team share bodies --confirm --token-stdin --workspace . --json

# Already-redacted origin bytes are a distinct signed representation.
# Switching an exact publication to already_redacted is refused; unshare first.
ee team share bodies --representation already_redacted --workspace . --json
ee team share bodies --confirm --representation already_redacted --workspace . --json

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

Foreground steward is enough for a two-node lab. After join, inbound
listen is `ee mesh hello-responder run --workspace .` (loads enrolled
peers). User-scoped install is optional and never requires root; the
loaded daemon starts that owner when mesh is on and peers exist.

```bash
ee team steward once --workspace . --json
ee mesh hello-responder run --workspace . --json
ee team projects reconcile --workspace . --json
ee team revoke --all-before-floor --workspace . --json
ee daemon install --confirm --json          # write launchd / systemd --user unit
ee daemon install --confirm --load --json   # also load the supervisor
```

## Custom port and multi-user limits

- The origin responder owns **one** TCP port for every registered
  workspace on the host. A second user cannot bind a second responder.
- Override the port only through the documented mesh config; do not
  start a second `ee` listener by hand.
- Windows can store team credentials and body-cache bytes through the
  reviewed SID/DACL/reparse adapter. The inbound responder stays Unix-only.

Repair commands always come from `ee team doctor --json`.
