# Local Two-Node Mesh Demo

Bead: `bd-ghey6`

`scripts/e2e_overhaul/mesh_local_two_node_demo.sh` is the deterministic
no-real-network SRR6 demo fixture. It models two stable nodes over local file
transport:

- `node01` remembers one procedural rule and exports metadata only.
- `node02` syncs that metadata into its local peer cache.
- Tier 1 search on `node02` returns the cached metadata without contacting a
  network.
- A lazy body fetch succeeds only after the trusted-body policy lane is granted.
- A fresher peer revision is reported as a notice and does not mutate the
  foreground result.
- A peer-unavailable path keeps the local metadata result usable.

Run it directly with:

```bash
EE_E2E_TMPDIR=/tmp scripts/e2e_overhaul/mesh_local_two_node_demo.sh
```

The script emits `ee.test_event.v1` JSONL through the shared e2e logger and
compares its generated summary to
`tests/fixtures/golden/mesh/local_two_node_demo.json`. It does not require a
Tailscale account, external network access, or a built `ee` binary.
