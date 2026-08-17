# Real Tailscale Surface Smoke

`scripts/e2e_overhaul/mesh_tailscale_smoke.sh` is an SRR6 opt-in local-surface
smoke for hosts that already have an authenticated `tailscaled` and at least
one visible peer on the same tailnet. It is **not** the two-human EE-to-EE
live-sync gate.

Unix production sync is live: `ee mesh sync --once` and `ee team steward once`
use `TcpMeshForegroundSyncTransport` (hello, EventFetch, grant-gated
BodyFetch). Isolated loopback / TeamJoin proofs live in
[`verification_matrix.md`](verification_matrix.md). This script does not
replace those proofs. It checks that local Tailscale observation, enrollment,
policy filtering, and file export/import still interoperate on a real tailnet
host.

Do not cite a green run of this script as evidence that two distinct humans
exchanged memory over Tailscale. That graduation is
`bd-tc-epic-qzk7o.3.8` (T2.6): opted-in two-node socket contact, cursors
advance, and US-4 search/pack recall teammate text.

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
- `ee mesh sync --once --json` completes one foreground supervisor cycle.
  With no second `ee` process on the selected peer, EventFetch/BodyFetch may
  still contact zero peers; that is a missing remote `ee`, not a no-op
  transport. Placeholder values such as `bodyFetchMs=0` and
  `not_exercised_by_local_sync_surface` are not network transfer evidence.
- `ee mesh export --peer <enrolled-peer-id>` produces a peer-policy-filtered
  JSON artifact and local file-based `ee mesh import --dry-run` accepts it.
- The event log includes redaction-safe peer, route, latency, policy, and
  revision fields for local-surface closeout evidence.

Artifacts are retained. Do not clean them up during agent sessions unless the
user explicitly authorizes the exact deletion.
