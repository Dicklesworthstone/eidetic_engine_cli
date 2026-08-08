# Real Tailscale Surface Smoke

`scripts/e2e_overhaul/mesh_tailscale_smoke.sh` is an SRR6 opt-in local-surface
smoke for hosts that already have an authenticated `tailscaled` and at least
one visible peer on the same tailnet. It is **not** an EE-to-EE transport proof.
The current production foreground supervisor uses a no-op transport, so this
script can complete without contacting another EE process, exchanging a hello,
fetching a body, or advancing an anti-entropy cursor over the network.

Treat its result as evidence that local Tailscale observation, enrollment,
policy filtering, and file export/import interoperate on a real tailnet host.
Do not cite it as evidence of live mesh synchronization. That graduation
requires the authenticated two-node socket and live-tailnet gates tracked by
`bd-tc-epic-qzk7o.3.7` and `bd-tc-epic-qzk7o.3.8`.

Default CI and ordinary agent verification must not run a real tailnet probe:

```bash
scripts/e2e_overhaul/mesh_tailscale_smoke.sh
# exits 78 and writes an ee.test_event.v1 skip record
```

To run it manually:

```bash
EE_E2E_REAL_TAILSCALE=1 \
EE_REAL_TAILSCALE_PEER='<node-key-or-magicdns-or-tailscale-ip>' \
EE_BINARY=/path/to/ee \
scripts/e2e_overhaul/mesh_tailscale_smoke.sh
```

The script validates:

- `tailscale status --json` is available and includes the requested peer.
- `ee mesh status --json` runs with `EE_MESH_ENABLED=1`.
- The selected real peer is explicitly enrolled and its returned opaque
  `peerId` is used as the transfer target.
- `ee mesh sync --once --json` completes one foreground supervisor cycle; the
  current no-op transport permits zero peer contacts.
- `ee mesh export --peer <enrolled-peer-id>` produces a peer-policy-filtered
  JSON artifact and local file-based `ee mesh import --dry-run` accepts it.
- The event log includes redaction-safe peer, route, latency, policy, and
  revision fields for local-surface closeout evidence. Placeholder values such
  as `bodyFetchMs=0` and `not_exercised_by_local_sync_surface` are not network
  transfer evidence.

Artifacts are retained. Do not clean them up during agent sessions unless the
user explicitly authorizes the exact deletion.
