# Real Tailscale Smoke

`scripts/e2e_overhaul/mesh_tailscale_smoke.sh` is the SRR6 opt-in transport
smoke for hosts that already have an authenticated `tailscaled` and at least one
visible peer on the same tailnet.

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
- `ee mesh sync --once --json` completes one foreground supervisor cycle.
- `ee mesh export` produces a JSON artifact and `ee mesh import --dry-run`
  accepts it.
- The event log includes redaction-safe peer, route, latency, policy, and
  revision fields for closeout evidence.

Artifacts are retained. Do not clean them up during agent sessions unless the
user explicitly authorizes the exact deletion.
